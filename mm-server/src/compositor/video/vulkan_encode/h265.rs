// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use anyhow::{bail, Context};
use ash::vk;
use ash::vk::native::{
    StdVideoEncodeH265SliceSegmentHeader,
    StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_420, StdVideoH265DecPicBufMgr,
    StdVideoH265PictureParameterSet, StdVideoH265ProfileTierLevel,
    StdVideoH265SequenceParameterSet, StdVideoH265SequenceParameterSetVui,
    StdVideoH265ShortTermRefPicSet, StdVideoH265VideoParameterSet,
};
use bytes::Bytes;
use tracing::trace;

use crate::compositor::AttachedClients;
use crate::vulkan::*;

use super::gop_structure::HierarchicalP;

vk_chain! {
    pub struct H265EncodeProfile<'a> {
        pub profile_info: vk::VideoProfileInfoKHR<'a>,
        pub encode_usage_info: vk::VideoEncodeUsageInfoKHR<'a>,
        pub h265_profile: vk::VideoEncodeH265ProfileInfoEXT<'a>,
    }
}

vk_chain! {
    pub struct H265EncodeCapabilities<'a> {
        pub video_caps: vk::VideoCapabilitiesKHR<'a>,
        pub encode_caps: vk::VideoEncodeCapabilitiesKHR<'a>,
        pub h265_caps: vk::VideoEncodeH265CapabilitiesEXT<'a>,
    }
}

vk_chain! {
    pub struct H265QualityLevelProperties<'a> {
        pub props: vk::VideoEncodeQualityLevelPropertiesKHR<'a>,
        pub h265_props: vk::VideoEncodeH265QualityLevelPropertiesEXT<'a>,
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct H265Metadata {
    pic_type: u32,
    pic_order_cnt: i32,
}

pub struct H265Encoder {
    inner: super::EncoderInner,
    profile: H265EncodeProfile,
    rc_mode: vk::VideoEncodeRateControlModeFlagsKHR,

    layers: u32,
    structure: HierarchicalP,
    pic_metadata: Vec<H265Metadata>, // Indexed by layer.
    idr_num: u32,
    frame_num: u32,

    headers: Bytes,
}

impl H265Encoder {
    pub fn new(
        vk: Arc<VkContext>,
        attached_clients: AttachedClients,
        stream_seq: u64,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Self> {
        let (video_loader, encode_loader) = vk.video_apis.as_ref().unwrap();

        let profile_idc = 1; // Main profile.
        let h265_profile_info =
            vk::VideoEncodeH265ProfileInfoEXT::default().std_profile_idc(profile_idc);

        let mut profile = H265EncodeProfile::new(
            super::default_profile(vk::VideoCodecOperationFlagsKHR::ENCODE_H265_EXT),
            super::default_encode_usage(),
            h265_profile_info,
        );

        let mut caps = H265EncodeCapabilities::default();

        unsafe {
            video_loader
                .get_physical_device_video_capabilities(
                    vk.device_info.pdevice,
                    &profile.profile_info,
                    caps.as_mut(),
                )
                .context("vkGetPhysicalDeviceVideoCapabilitiesKHR")?;
        };

        trace!("video capabilities: {:#?}", caps.video_caps);
        trace!("encode capabilities: {:#?}", caps.encode_caps);
        trace!("h265 capabilities: {:#?}", caps.h265_caps);

        // let quality_level = caps.encode_caps.max_quality_levels - 1;
        // let mut quality_props = H265QualityLevelProperties::default();

        // unsafe {
        //     let get_info = vk::PhysicalDeviceVideoEncodeQualityLevelInfoKHR::default()
        //         .video_profile(&profile.profile_info)
        //         .quality_level(quality_level);

        //     encode_loader.get_physical_device_video_encode_quality_level_properties(
        //         vk.device_info.pdevice,
        //         &get_info,
        //         quality_props.as_mut(),
        //     )?;
        // }

        // trace!("quality level properties: {:#?}", quality_props.props);
        // trace!(
        //     "h265 quality level properties: {:#?}",
        //     quality_props.h265_props
        // );

        let mut rc_mode = vk::VideoEncodeRateControlModeFlagsKHR::DISABLED;
        if !caps.encode_caps.rate_control_modes.contains(rc_mode) {
            rc_mode = vk::VideoEncodeRateControlModeFlagsKHR::DEFAULT;
        }

        let mut layers = std::cmp::min(4, caps.h265_caps.max_sub_layer_count);
        let mut structure = HierarchicalP::new(layers as u32, super::DEFAULT_GOP_SIZE);
        while structure.required_dpb_size() as u32 > caps.video_caps.max_dpb_slots {
            layers -= 1;
            if layers == 0 {
                bail!("max_dpb_slots too low");
            }

            structure = HierarchicalP::new(layers as u32, super::DEFAULT_GOP_SIZE);
        }

        // TODO check more caps
        // TODO autoselect level
        let level_idc = vk::native::StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_5_2;
        if caps.h265_caps.max_level_idc != 0 && caps.h265_caps.max_level_idc < level_idc {
            bail!("video resolution too large for hardware");
        }

        const CTB_SIZES: [(vk::VideoEncodeH265CtbSizeFlagsEXT, usize); 3] = [
            (vk::VideoEncodeH265CtbSizeFlagsEXT::TYPE_16, 16),
            (vk::VideoEncodeH265CtbSizeFlagsEXT::TYPE_32, 32),
            (vk::VideoEncodeH265CtbSizeFlagsEXT::TYPE_64, 64),
        ];

        let min_ctb = CTB_SIZES
            .iter()
            .filter(|(flag, _)| caps.h265_caps.ctb_sizes.contains(*flag))
            .map(|(_, size)| *size)
            .min()
            .expect("no ctb size found");

        let max_ctb = CTB_SIZES
            .iter()
            .filter(|(flag, _)| caps.h265_caps.ctb_sizes.contains(*flag))
            .map(|(_, size)| *size)
            .max()
            .expect("no ctb size found");

        const TBS_SIZES: [(vk::VideoEncodeH265TransformBlockSizeFlagsEXT, usize); 4] = [
            (vk::VideoEncodeH265TransformBlockSizeFlagsEXT::TYPE_4, 4),
            (vk::VideoEncodeH265TransformBlockSizeFlagsEXT::TYPE_8, 8),
            (vk::VideoEncodeH265TransformBlockSizeFlagsEXT::TYPE_16, 16),
            (vk::VideoEncodeH265TransformBlockSizeFlagsEXT::TYPE_32, 32),
        ];

        let min_tbs = TBS_SIZES
            .iter()
            .filter(|(flag, _)| caps.h265_caps.transform_block_sizes.contains(*flag))
            .map(|(_, size)| *size)
            .min()
            .expect("no tbs size found");

        let max_tbs = TBS_SIZES
            .iter()
            .filter(|(flag, _)| caps.h265_caps.transform_block_sizes.contains(*flag))
            .map(|(_, size)| *size)
            .max()
            .expect("no tbs size found");

        let aligned_width = width.next_multiple_of(min_ctb as u32);
        let aligned_height = height.next_multiple_of(min_ctb as u32);

        trace!(
            min_ctb,
            max_ctb,
            min_tbs,
            max_tbs,
            aligned_width,
            aligned_height,
            "block sizes",
        );

        // Divide by two because of chroma subsampling, I guess?
        let crop_right = (aligned_width - width) / 2;
        let crop_bottom = (aligned_height - height) / 2;

        trace!("crop right: {}, bottom: {}", crop_right, crop_bottom);

        let mut vui = StdVideoH265SequenceParameterSetVui {
            // BT.709.
            colour_primaries: 1,
            transfer_characteristics: 1,
            matrix_coeffs: 1,
            // Unspecified.
            video_format: 5,
            ..unsafe { std::mem::zeroed() }
        };

        vui.flags.set_video_signal_type_present_flag(1);
        vui.flags.set_video_full_range_flag(0); // Narrow range.
        vui.flags.set_colour_description_present_flag(1);

        let ptl = StdVideoH265ProfileTierLevel {
            general_profile_idc: profile_idc,
            general_level_idc: level_idc,
            ..unsafe { std::mem::zeroed() }
        };

        // ptl.flags.set_general_progressive_source_flag(1);
        // ptl.flags.set_general_interlaced_source_flag(0);

        let layers_minus_1 = (layers - 1) as u8;
        let mut pbm: StdVideoH265DecPicBufMgr = unsafe { std::mem::zeroed() };
        pbm.max_dec_pic_buffering_minus1[layers_minus_1 as usize] =
            (structure.required_dpb_size() - 1) as u8;
        // No picture reordering.
        pbm.max_num_reorder_pics[layers_minus_1 as usize] = 0;
        pbm.max_latency_increase_plus1[layers_minus_1 as usize] = 0;

        let mut vps = StdVideoH265VideoParameterSet {
            vps_max_sub_layers_minus1: layers_minus_1,
            pDecPicBufMgr: &pbm,
            pHrdParameters: std::ptr::null(),
            pProfileTierLevel: &ptl,
            ..unsafe { std::mem::zeroed() }
        };

        vps.flags.set_vps_sub_layer_ordering_info_present_flag(1);
        vps.flags.set_vps_temporal_id_nesting_flag(1);

        let min_cb = 8_u8;
        let max_cb = max_ctb;

        let max_transform_hierarchy_depth = (max_ctb.ilog2() - min_tbs.ilog2()) as u8;

        let mut sps = StdVideoH265SequenceParameterSet {
            chroma_format_idc: StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_420,
            pic_width_in_luma_samples: aligned_width,
            pic_height_in_luma_samples: aligned_height,
            sps_max_sub_layers_minus1: layers_minus_1,
            bit_depth_luma_minus8: 0,   // TODO HDR
            bit_depth_chroma_minus8: 0, // TODO HDR
            log2_max_pic_order_cnt_lsb_minus4: 4,
            log2_min_luma_coding_block_size_minus3: (min_cb.ilog2() - 3) as u8,
            log2_diff_max_min_luma_coding_block_size: (max_cb.ilog2() - min_cb.ilog2()) as u8,
            log2_min_luma_transform_block_size_minus2: (min_tbs.ilog2() - 2) as u8,
            log2_diff_max_min_luma_transform_block_size: (max_tbs.ilog2() - min_tbs.ilog2()) as u8,
            max_transform_hierarchy_depth_inter: max_transform_hierarchy_depth,
            max_transform_hierarchy_depth_intra: max_transform_hierarchy_depth,
            conf_win_right_offset: crop_right,
            conf_win_bottom_offset: crop_bottom,
            pProfileTierLevel: &ptl,
            pDecPicBufMgr: &pbm,
            pSequenceParameterSetVui: &vui,
            ..unsafe { std::mem::zeroed() }
        };

        sps.flags.set_conformance_window_flag(1);
        sps.flags.set_vui_parameters_present_flag(1);
        sps.flags.set_sps_temporal_id_nesting_flag(1);
        sps.flags.set_sps_sub_layer_ordering_info_present_flag(1);

        let pps = StdVideoH265PictureParameterSet {
            ..unsafe { std::mem::zeroed() }
        };

        let sps = [sps];
        let pps = [pps];
        let vps = [vps];

        let h265_add_info = vk::VideoEncodeH265SessionParametersAddInfoEXT::default()
            .std_vp_ss(&vps)
            .std_sp_ss(&sps)
            .std_pp_ss(&pps);
        let mut session_params = vk::VideoEncodeH265SessionParametersCreateInfoEXT::default()
            .parameters_add_info(&h265_add_info)
            .max_std_vps_count(1)
            .max_std_pps_count(1)
            .max_std_sps_count(1);

        let inner = super::EncoderInner::new(
            vk.clone(),
            attached_clients,
            stream_seq,
            width,
            height,
            structure.required_dpb_size(),
            profile.as_mut(),
            caps.video_caps,
            &mut session_params,
        )?;

        // Generate encoded stream headers.
        let headers = unsafe {
            let mut h265_get_info = vk::VideoEncodeH265SessionParametersGetInfoEXT::default()
                .write_std_vps(true)
                .write_std_sps(true)
                .write_std_pps(true);

            let mut h265_feedback_info =
                vk::VideoEncodeH265SessionParametersFeedbackInfoEXT::default();

            let mut feedback_info = vk::VideoEncodeSessionParametersFeedbackInfoKHR::default()
                .push_next(&mut h265_feedback_info);

            let get_info = vk::VideoEncodeSessionParametersGetInfoKHR::default()
                .video_session_parameters(inner.session_params)
                .push_next(&mut h265_get_info);

            encode_loader
                .get_encoded_video_session_parameters(&get_info, &mut feedback_info)
                .context("vkGetEncodedVideoSessionParametersKHR")?
        };

        if headers.is_empty() {
            bail!("failed to generate sps/pps/vps");
        } else {
            trace!("generated {} bytes of h265 headers", headers.len());
        }

        let pic_metadata = vec![H265Metadata::default(); layers as usize];

        Ok(Self {
            inner,
            profile,
            rc_mode,
            layers,
            structure,
            pic_metadata,
            idr_num: 0,
            frame_num: 0,
            headers: Bytes::copy_from_slice(&headers),
        })
    }

    pub unsafe fn submit_encode(
        &mut self,
        input: &VkImage,
        semaphore: vk::Semaphore,
        tp_acquire: u64,
        tp_release: u64,
    ) -> anyhow::Result<()> {
        let frame_state = self.structure.next_frame();
        if frame_state.is_keyframe {
            self.idr_num += 1;
            self.frame_num = 0;
        }

        let pattern = if self.layers > 1 {
            vk::VideoEncodeH265RateControlFlagsEXT::TEMPORAL_SUB_LAYER_PATTERN_DYADIC
        } else {
            vk::VideoEncodeH265RateControlFlagsEXT::REFERENCE_PATTERN_FLAT
        };

        let mut h265_rc_info = vk::VideoEncodeH265RateControlInfoEXT::default()
            .gop_frame_count(super::DEFAULT_GOP_SIZE)
            .idr_period(super::DEFAULT_GOP_SIZE)
            .consecutive_b_frame_count(0)
            .sub_layer_count(self.layers)
            .flags(vk::VideoEncodeH265RateControlFlagsEXT::REGULAR_GOP | pattern);

        let mut rc_info =
            vk::VideoEncodeRateControlInfoKHR::default().rate_control_mode(self.rc_mode);

        // Doesn't have a push_next method, because we're supposed to call it on
        // the parent struct.
        rc_info.p_next = <*mut _>::cast(&mut h265_rc_info);

        let weight_table: vk::native::StdVideoEncodeH265WeightTable = std::mem::zeroed();

        let slice_type = if frame_state.is_keyframe {
            vk::native::StdVideoH265SliceType_STD_VIDEO_H265_SLICE_TYPE_I
        } else {
            vk::native::StdVideoH265SliceType_STD_VIDEO_H265_SLICE_TYPE_P
        };

        let pic_type = if frame_state.is_keyframe {
            vk::native::StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_IDR
        } else {
            vk::native::StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_P
        };

        let std_slice_header = StdVideoEncodeH265SliceSegmentHeader {
            slice_type,
            pWeightTable: &weight_table,
            MaxNumMergeCand: 5, // Decoders complain if this is zero. The max value is 5.
            ..std::mem::zeroed()
        };

        let slice_segment_info = [vk::VideoEncodeH265NaluSliceSegmentInfoEXT::default()
            .std_slice_segment_header(&std_slice_header)
            .constant_qp(
                if self.rc_mode == vk::VideoEncodeRateControlModeFlagsKHR::DISABLED {
                    35
                } else {
                    0
                },
            )];

        let mut ref_lists_info = vk::native::StdVideoEncodeH265ReferenceListsInfo {
            RefPicList0: [u8::MAX; 15],
            RefPicList1: [u8::MAX; 15],
            ..std::mem::zeroed()
        };

        // Point to the references.
        let ref_pics = frame_state
            .ref_ids
            .iter()
            .map(|id| {
                self.inner
                    .dpb
                    .get_pic(*id)
                    .ok_or(anyhow::anyhow!("ref pic {id} missing from dpb",))
            })
            .collect::<Result<Vec<_>, _>>()?;

        for (idx, pic) in ref_pics.iter().enumerate() {
            ref_lists_info.RefPicList0[idx] = pic.index as u8;
        }

        let mut short_term_refs = StdVideoH265ShortTermRefPicSet {
            used_by_curr_pic_s0_flag: if ref_pics.is_empty() { 0 } else { 1 },
            num_negative_pics: ref_pics.len() as u8,
            used_by_curr_pic_s1_flag: 0, // No forward refs.
            num_positive_pics: 0,
            ..std::mem::zeroed()
        };

        let pic_order_cnt = frame_state.gop_position as i32;
        for (idx, id) in frame_state.ref_ids.iter().enumerate() {
            short_term_refs.delta_poc_s0_minus1[idx] =
                (pic_order_cnt - self.pic_metadata[*id as usize].pic_order_cnt - 1) as u16;
        }

        let mut std_pic_info = vk::native::StdVideoEncodeH265PictureInfo {
            pic_type,
            sps_video_parameter_set_id: 0,
            pps_seq_parameter_set_id: 0,
            pps_pic_parameter_set_id: 0,
            PicOrderCntVal: frame_state.gop_position as i32,
            TemporalId: frame_state.id as u8,
            pRefLists: &ref_lists_info,
            pShortTermRefPicSet: &short_term_refs,
            ..std::mem::zeroed()
        };

        std_pic_info
            .flags
            .set_IrapPicFlag(frame_state.is_keyframe as u32);
        std_pic_info
            .flags
            .set_is_reference(frame_state.is_reference as u32);

        if frame_state.is_keyframe {
            std_pic_info.flags.set_pic_output_flag(1);
            std_pic_info.flags.set_no_output_of_prior_pics_flag(1);
        }

        let mut h265_pic_info = vk::VideoEncodeH265PictureInfoEXT::default()
            .std_picture_info(&std_pic_info)
            .nalu_slice_segment_entries(&slice_segment_info);

        let mut std_ref_infos = frame_state
            .ref_ids
            .iter()
            .map(|id| vk::native::StdVideoEncodeH265ReferenceInfo {
                pic_type: self.pic_metadata[*id as usize].pic_type,
                PicOrderCntVal: self.pic_metadata[*id as usize].pic_order_cnt,
                TemporalId: *id as u8,
                ..std::mem::zeroed()
            })
            .collect::<Vec<_>>();

        let mut ref_info = std_ref_infos
            .iter_mut()
            .map(|info| vk::VideoEncodeH265DpbSlotInfoEXT::default().std_reference_info(info))
            .collect::<Vec<_>>();

        let setup_std_ref_info = vk::native::StdVideoEncodeH265ReferenceInfo {
            pic_type,
            PicOrderCntVal: pic_order_cnt,
            TemporalId: frame_state.id as u8,
            ..std::mem::zeroed()
        };

        let mut setup_info =
            vk::VideoEncodeH265DpbSlotInfoEXT::default().std_reference_info(&setup_std_ref_info);

        let insert = if frame_state.stream_position == 0 {
            Some(self.headers.clone())
        } else {
            None
        };

        self.inner.submit_encode(
            input,
            semaphore,
            tp_acquire,
            tp_release,
            &frame_state,
            &mut rc_info,
            &mut h265_pic_info,
            &mut setup_info,
            &mut ref_info,
            insert,
        )?;

        // Save the reference info for the DPB slot we just wrote.
        self.pic_metadata[frame_state.id as usize] = H265Metadata {
            pic_type,
            pic_order_cnt,
        };

        // This is supposed to increment only for reference frames.
        if frame_state.is_reference {
            self.frame_num += 1;
        }

        Ok(())
    }

    pub fn input_format(&self) -> vk::Format {
        self.inner.input_format
    }

    pub fn create_encode_image(&mut self) -> anyhow::Result<VkImage> {
        self.inner.create_encode_image(self.profile.as_mut())
    }
}
