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
use tracing::{debug, trace};

use super::gop_structure::HierarchicalP;
use super::rate_control::{self, RateControlMode};
use crate::codec::VideoCodec;
use crate::color::VideoProfile;
use crate::{
    compositor::{CompositorHandle, VideoStreamParams},
    vulkan::*,
};

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
    ref_count: u32,
}

pub struct H265Encoder {
    inner: super::EncoderInner,
    profile: H265EncodeProfile,
    rc_mode: super::rate_control::RateControlMode,

    structure: HierarchicalP,
    pic_metadata: Vec<H265Metadata>, // Indexed by layer.
    idr_num: u32,
    frame_num: u32,

    headers: Bytes,
}

impl H265Encoder {
    pub fn new(
        vk: Arc<VkContext>,
        compositor: CompositorHandle,
        stream_seq: u64,
        params: VideoStreamParams,
        framerate: u32,
    ) -> anyhow::Result<Self> {
        let (video_loader, encode_loader) = vk.video_apis.as_ref().unwrap();

        let op = vk::VideoCodecOperationFlagsKHR::ENCODE_H265_EXT;
        let (profile, profile_idc) = match params.profile {
            VideoProfile::Hd => (super::default_profile(op), 1), // Main
            VideoProfile::Hdr10 => (super::default_hdr10_profile(op), 2), // Main10
        };

        let h265_profile_info =
            vk::VideoEncodeH265ProfileInfoEXT::default().std_profile_idc(profile_idc);

        let mut profile = H265EncodeProfile::new(
            profile,
            super::default_encode_usage(vk.device_info.driver_version.clone()),
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

        let quality_level = caps.encode_caps.max_quality_levels - 1;
        let mut quality_props = H265QualityLevelProperties::default();

        unsafe {
            let get_info = vk::PhysicalDeviceVideoEncodeQualityLevelInfoKHR::default()
                .video_profile(&profile.profile_info)
                .quality_level(quality_level);

            encode_loader.get_physical_device_video_encode_quality_level_properties(
                vk.device_info.pdevice,
                &get_info,
                quality_props.as_mut(),
            )?;
        }

        trace!("quality level properties: {:#?}", quality_props.props);
        trace!(
            "h265 quality level properties: {:#?}",
            quality_props.h265_props
        );

        let structure = super::default_structure(
            VideoCodec::H265,
            caps.h265_caps.max_sub_layer_count,
            caps.video_caps.max_dpb_slots,
        )?;

        let rc_mode = rate_control::select_rc_mode(
            params,
            &caps.encode_caps,
            caps.h265_caps.min_qp.try_into().unwrap_or(17),
            caps.h265_caps.max_qp.try_into().unwrap_or(50),
            &structure,
        );

        debug!(?rc_mode, "selected rate control mode");

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

        let aligned_width = params
            .width
            .next_multiple_of(caps.encode_caps.encode_input_picture_granularity.width);
        let aligned_height = params
            .height
            .next_multiple_of(caps.encode_caps.encode_input_picture_granularity.height);

        trace!(
            min_ctb,
            max_ctb,
            min_tbs,
            max_tbs,
            aligned_width,
            aligned_height,
            "block sizes",
        );

        let crop_right = (aligned_width - params.width) / 2;
        let crop_bottom = (aligned_height - params.height) / 2;

        trace!("crop right: {}, bottom: {}", crop_right, crop_bottom);

        let (colour_primaries, transfer_characteristics, matrix_coeffs) = match params.profile {
            VideoProfile::Hd => (1, 1, 1),
            VideoProfile::Hdr10 => (9, 16, 9),
        };

        let mut vui = StdVideoH265SequenceParameterSetVui {
            colour_primaries,
            transfer_characteristics,
            matrix_coeffs,
            // Unspecified.
            video_format: 5,
            ..unsafe { std::mem::zeroed() }
        };

        vui.flags.set_video_signal_type_present_flag(1);
        vui.flags.set_colour_description_present_flag(1);
        vui.flags.set_video_full_range_flag(0); // Narrow range.

        let ptl = StdVideoH265ProfileTierLevel {
            general_profile_idc: profile_idc,
            general_level_idc: level_idc,
            ..unsafe { std::mem::zeroed() }
        };

        // ptl.flags.set_general_progressive_source_flag(1);
        // ptl.flags.set_general_interlaced_source_flag(0);

        let layers_minus_1 = (structure.layers - 1) as u8;
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

        let bit_depth = match params.profile {
            VideoProfile::Hd => 8,
            VideoProfile::Hdr10 => 10,
        };

        let mut sps = StdVideoH265SequenceParameterSet {
            chroma_format_idc: StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_420,
            pic_width_in_luma_samples: aligned_width,
            pic_height_in_luma_samples: aligned_height,
            sps_max_sub_layers_minus1: layers_minus_1,
            bit_depth_luma_minus8: bit_depth - 8,
            bit_depth_chroma_minus8: bit_depth - 8,
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

        if caps
            .h265_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsEXT::SAMPLE_ADAPTIVE_OFFSET_ENABLED_FLAG_SET)
        {
            sps.flags.set_sample_adaptive_offset_enabled_flag(1);
        }

        if caps
            .h265_caps
            .std_syntax_flags
            .contains(vk::VideoEncodeH265StdFlagsEXT::TRANSFORM_SKIP_ENABLED_FLAG_SET)
        {
            sps.flags.set_transform_skip_context_enabled_flag(1);
        }

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
            compositor,
            stream_seq,
            params.width,
            params.height,
            framerate,
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

        let pic_metadata = vec![H265Metadata::default(); structure.layers as usize];

        Ok(Self {
            inner,
            profile,
            rc_mode,
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
        tp_acquire: VkTimelinePoint,
        tp_release: VkTimelinePoint,
    ) -> anyhow::Result<()> {
        let frame_state = self.structure.next_frame();
        if frame_state.is_keyframe {
            self.idr_num += 1;
            self.frame_num = 0;
        }

        let pattern = if self.structure.layers > 1 {
            vk::VideoEncodeH265RateControlFlagsEXT::TEMPORAL_SUB_LAYER_PATTERN_DYADIC
        } else {
            vk::VideoEncodeH265RateControlFlagsEXT::REFERENCE_PATTERN_FLAT
        };

        let mut h265_rc_layers = Vec::new();
        let mut rc_layers = Vec::new();

        if let RateControlMode::Vbr(vbr) = self.rc_mode {
            let layer_settings = (0..self.structure.layers)
                .map(|layer| vbr.layer(layer))
                .collect::<Vec<_>>();

            for settings in &layer_settings {
                h265_rc_layers.push(
                    vk::VideoEncodeH265RateControlLayerInfoEXT::default()
                        .use_min_qp(true)
                        .use_max_qp(true)
                        .min_qp(vk::VideoEncodeH265QpEXT {
                            qp_i: settings.min_qp as i32,
                            qp_p: settings.min_qp as i32,
                            qp_b: settings.min_qp as i32,
                        })
                        .max_qp(vk::VideoEncodeH265QpEXT {
                            qp_i: settings.max_qp as i32,
                            qp_p: settings.max_qp as i32,
                            qp_b: settings.max_qp as i32,
                        }),
                );
            }

            for (layer, (settings, h265_rc_layer)) in layer_settings
                .iter()
                .zip(h265_rc_layers.iter_mut())
                .enumerate()
            {
                let (fps_numerator, fps_denominator) = self
                    .structure
                    .layer_framerate(layer as u32, self.inner.framerate);

                rc_layers.push(
                    vk::VideoEncodeRateControlLayerInfoKHR::default()
                        .max_bitrate(settings.peak_bitrate)
                        .average_bitrate(settings.average_bitrate)
                        .frame_rate_numerator(fps_numerator)
                        .frame_rate_denominator(fps_denominator)
                        .push_next(h265_rc_layer),
                );
            }
        }

        let mut h265_rc_info = vk::VideoEncodeH265RateControlInfoEXT::default()
            .gop_frame_count(self.structure.gop_size)
            .idr_period(self.structure.gop_size)
            .consecutive_b_frame_count(0)
            .sub_layer_count(rc_layers.len() as u32)
            .flags(vk::VideoEncodeH265RateControlFlagsEXT::REGULAR_GOP | pattern);

        let vbv_size = match self.rc_mode {
            RateControlMode::Vbr(settings) => settings.vbv_size_ms,
            _ => 0,
        };

        let mut rc_info = vk::VideoEncodeRateControlInfoKHR::default()
            .rate_control_mode(self.rc_mode.as_vk_flags())
            .virtual_buffer_size_in_ms(vbv_size);

        if !rc_layers.is_empty() {
            rc_info = rc_info.layers(&rc_layers);
        }

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
            .constant_qp(if let RateControlMode::ConstantQp(qp) = self.rc_mode {
                qp.layer(frame_state.id) as i32
            } else {
                0
            })];

        let mut ref_lists_info = vk::native::StdVideoEncodeH265ReferenceListsInfo {
            RefPicList0: [u8::MAX; 15],
            RefPicList1: [u8::MAX; 15],
            ..std::mem::zeroed()
        };

        for (idx, id) in frame_state.ref_ids.iter().enumerate() {
            let pic = self
                .inner
                .dpb
                .get_pic(*id)
                .ok_or(anyhow::anyhow!("ref pic {id} missing from dpb"))?;

            ref_lists_info.RefPicList0[idx] = pic.index as u8;
        }

        // For each frame, we have to tell the decoder which pictures will be
        // used as references in the future, in addition to those that are
        // references for this frame.
        let mut ref_ids = self
            .pic_metadata
            .iter_mut()
            .enumerate()
            .filter_map(|(id, md)| {
                let id = id as u32;
                if md.ref_count == 0 {
                    None
                } else if frame_state.ref_ids.contains(&id) {
                    md.ref_count -= 1;
                    Some((id, true))
                } else {
                    Some((id, false))
                }
            })
            .collect::<Vec<_>>();

        // Sort in descending order of POC.
        ref_ids.sort_by_key(|(id, _)| {
            std::cmp::Reverse(self.pic_metadata[*id as usize].pic_order_cnt)
        });

        let mut short_term_refs = StdVideoH265ShortTermRefPicSet {
            used_by_curr_pic_s0_flag: 0,
            num_negative_pics: ref_ids.len() as u8,
            // No forward refs.
            used_by_curr_pic_s1_flag: 0,
            num_positive_pics: 0,
            ..std::mem::zeroed()
        };

        let pic_order_cnt = frame_state.gop_position as i32;
        let mut delta_poc = 0;
        for (idx, (id, is_direct_ref)) in ref_ids.into_iter().enumerate() {
            delta_poc = (pic_order_cnt - self.pic_metadata[id as usize].pic_order_cnt) - delta_poc;
            short_term_refs.delta_poc_s0_minus1[idx] = (delta_poc - 1) as u16;
            if is_direct_ref {
                short_term_refs.used_by_curr_pic_s0_flag |= 1 << idx;
            }
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
            .set_is_reference((frame_state.forward_ref_count > 0) as u32);

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
            ref_count: frame_state.forward_ref_count,
        };

        // This is supposed to increment only for reference frames.
        if frame_state.forward_ref_count > 0 {
            self.frame_num += 1;
        }

        Ok(())
    }

    pub fn input_format(&self) -> vk::Format {
        self.inner.input_format
    }

    pub fn create_input_image(&mut self) -> anyhow::Result<VkImage> {
        self.inner.create_input_image(self.profile.as_mut())
    }
}
