// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use anyhow::{bail, Context};
use ash::vk;
use ash::vk::native::{
    StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,
    StdVideoH264PictureParameterSet, StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_0,
    StdVideoH264SequenceParameterSet, StdVideoH264SequenceParameterSetVui,
};
use bytes::Bytes;
use tracing::{debug, trace};

use super::gop_structure::HierarchicalP;
use super::rate_control::{self, RateControlMode};
use crate::codec::VideoCodec;
use crate::{color::VideoProfile, session::control::VideoStreamParams, vulkan::*};

vk_chain! {
    pub struct H264EncodeProfile<'a> {
        pub profile_info: vk::VideoProfileInfoKHR<'a>,
        pub encode_usage_info: vk::VideoEncodeUsageInfoKHR<'a>,
        pub h264_profile: vk::VideoEncodeH264ProfileInfoEXT<'a>,
    }
}

vk_chain! {
    pub struct H264EncodeCapabilities<'a> {
        pub video_caps: vk::VideoCapabilitiesKHR<'a>,
        pub encode_caps: vk::VideoEncodeCapabilitiesKHR<'a>,
        pub h264_caps: vk::VideoEncodeH264CapabilitiesEXT<'a>,
    }
}

vk_chain! {
    pub struct H264QualityLevelProperties<'a> {
        pub props: vk::VideoEncodeQualityLevelPropertiesKHR<'a>,
        pub h264_props: vk::VideoEncodeH264QualityLevelPropertiesEXT<'a>,
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct H264Metadata {
    frame_num: u32,
    pic_order_cnt: i32,
}

pub struct H264Encoder {
    inner: super::EncoderInner,
    profile: H264EncodeProfile,
    rc_mode: RateControlMode,

    structure: HierarchicalP,
    pic_metadata: Vec<H264Metadata>, // Indexed by layer.
    idr_num: u32,
    frame_num: u32,

    headers: Bytes,
}

impl H264Encoder {
    pub fn new(
        vk: Arc<VkContext>,
        params: VideoStreamParams,
        framerate: u32,
        sink: impl super::Sink,
    ) -> anyhow::Result<Self> {
        let (video_loader, encode_loader) = vk.video_apis.as_ref().unwrap();

        let op = vk::VideoCodecOperationFlagsKHR::ENCODE_H264_EXT;
        let (profile, profile_idc) = match params.profile {
            VideoProfile::Hd => (super::default_profile(op), 100),
            VideoProfile::Hdr10 => (super::default_hdr10_profile(op), 110),
        };

        let h264_profile_info =
            vk::VideoEncodeH264ProfileInfoEXT::default().std_profile_idc(profile_idc);

        let mut profile = H264EncodeProfile::new(
            profile,
            super::default_encode_usage(vk.device_info.driver_version.clone()),
            h264_profile_info,
        );

        let mut caps = H264EncodeCapabilities::default();

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
        trace!("h264 capabilities: {:#?}", caps.h264_caps);

        // unsafe {
        //     let get_info =
        // vk::PhysicalDeviceVideoEncodeQualityLevelInfoKHR::default()
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
        //     "h264 quality level properties: {:#?}",
        //     quality_props.h264_props
        // );

        let structure = super::default_structure(
            VideoCodec::H264,
            caps.h264_caps
                .max_temporal_layer_count
                .min(caps.encode_caps.max_rate_control_layers),
            caps.video_caps.max_dpb_slots,
        )?;

        let rc_mode = rate_control::select_rc_mode(
            params,
            &caps.encode_caps,
            caps.h264_caps.min_qp.try_into().unwrap_or(17),
            caps.h264_caps.max_qp.try_into().unwrap_or(50),
            &structure,
        );
        debug!(?rc_mode, "selected rate control mode");

        // TODO check more caps
        // TODO autoselect level
        let level_idc = vk::native::StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_5_2;
        if caps.h264_caps.max_level_idc != 0 && caps.h264_caps.max_level_idc < level_idc {
            bail!("video resolution too large for hardware");
        }

        assert_eq!(
            caps.video_caps.picture_access_granularity.width,
            caps.video_caps.picture_access_granularity.height
        );

        let mb_width = caps.video_caps.picture_access_granularity.width;
        let mb_height = caps.video_caps.picture_access_granularity.height;
        trace!("mb size: {mb_width}x{mb_height}");

        let aligned_width = params.width.next_multiple_of(mb_width);
        let aligned_height = params.height.next_multiple_of(mb_height);

        trace!(
            "aligned width: {}, height: {}",
            aligned_width,
            aligned_height
        );

        // Divide by two because of chroma subsampling, I guess?
        let crop_right = (aligned_width - params.width) / 2;
        let crop_bottom = (aligned_height - params.height) / 2;

        trace!("crop right: {}, bottom: {}", crop_right, crop_bottom);

        let (colour_primaries, transfer_characteristics, matrix_coefficients) = match params.profile
        {
            VideoProfile::Hd => (1, 1, 1),
            VideoProfile::Hdr10 => (9, 16, 9),
        };

        let mut vui = StdVideoH264SequenceParameterSetVui {
            colour_primaries,
            transfer_characteristics,
            matrix_coefficients,
            // Unspecified.
            video_format: 5,
            ..unsafe { std::mem::zeroed() }
        };

        vui.flags.set_video_signal_type_present_flag(1);
        vui.flags.set_video_full_range_flag(0); // Narrow range.
        vui.flags.set_color_description_present_flag(1);

        let log2_max_frame_num_minus4 = structure
            .gop_size
            .next_power_of_two()
            .ilog2()
            .saturating_sub(4) as u8;

        let bit_depth = match params.profile {
            VideoProfile::Hd => 8,
            VideoProfile::Hdr10 => 10,
        };

        let mut sps = StdVideoH264SequenceParameterSet {
            profile_idc,
            level_idc,
            chroma_format_idc: StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420,

            bit_depth_chroma_minus8: bit_depth - 8,
            bit_depth_luma_minus8: bit_depth - 8,

            max_num_ref_frames: 1,
            pic_order_cnt_type: StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_0,
            log2_max_pic_order_cnt_lsb_minus4: log2_max_frame_num_minus4,
            log2_max_frame_num_minus4,
            pic_width_in_mbs_minus1: (aligned_width / mb_width) - 1,
            pic_height_in_map_units_minus1: (aligned_height / mb_height) - 1,
            frame_crop_right_offset: crop_right,
            frame_crop_bottom_offset: crop_bottom,

            pSequenceParameterSetVui: <*const _>::cast(&vui),
            ..unsafe { std::mem::zeroed() }
        };

        sps.flags.set_vui_parameters_present_flag(1);
        sps.flags.set_frame_mbs_only_flag(1);
        if crop_right > 0 || crop_bottom > 0 {
            sps.flags.set_frame_cropping_flag(1);
        }

        let pps = StdVideoH264PictureParameterSet {
            ..unsafe { std::mem::zeroed() }
        };

        let sps = [sps];
        let pps = [pps];

        let h264_add_info = vk::VideoEncodeH264SessionParametersAddInfoEXT::default()
            .std_sp_ss(&sps)
            .std_pp_ss(&pps);
        let mut session_params = vk::VideoEncodeH264SessionParametersCreateInfoEXT::default()
            .parameters_add_info(&h264_add_info)
            .max_std_pps_count(1)
            .max_std_sps_count(1);

        let inner = super::EncoderInner::new(
            vk.clone(),
            params.width,
            params.height,
            framerate,
            structure.required_dpb_size(),
            profile.as_mut(),
            caps.video_caps,
            &mut session_params,
            sink,
        )?;

        // Generate encoded stream headers.
        let headers = unsafe {
            let mut h264_get_info = vk::VideoEncodeH264SessionParametersGetInfoEXT::default()
                .write_std_sps(true)
                .write_std_pps(true);

            let mut h264_feedback_info =
                vk::VideoEncodeH264SessionParametersFeedbackInfoEXT::default();

            let mut feedback_info = vk::VideoEncodeSessionParametersFeedbackInfoKHR::default()
                .push_next(&mut h264_feedback_info);

            let get_info = vk::VideoEncodeSessionParametersGetInfoKHR::default()
                .video_session_parameters(inner.session_params)
                .push_next(&mut h264_get_info);

            encode_loader
                .get_encoded_video_session_parameters(&get_info, &mut feedback_info)
                .context("vkGetEncodedVideoSessionParametersKHR")?
        };

        if headers.is_empty() {
            bail!("failed to generate sps/pps");
        } else {
            trace!("generated {} bytes of h264 headers", headers.len());
        }

        let pic_metadata = vec![H264Metadata::default(); structure.layers as usize];

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
        }

        if frame_state.gop_position == 0 {
            self.frame_num = 0;
        }

        let pattern = if self.structure.layers > 1 {
            vk::VideoEncodeH264RateControlFlagsEXT::TEMPORAL_LAYER_PATTERN_DYADIC
        } else {
            vk::VideoEncodeH264RateControlFlagsEXT::REFERENCE_PATTERN_FLAT
        };

        let mut h264_rc_layers = Vec::new();
        let mut rc_layers = Vec::new();

        if let RateControlMode::Vbr(vbr) = self.rc_mode {
            let layer_settings = (0..self.structure.layers)
                .map(|layer| vbr.layer(layer))
                .collect::<Vec<_>>();

            for settings in &layer_settings {
                h264_rc_layers.push(
                    vk::VideoEncodeH264RateControlLayerInfoEXT::default()
                        .use_min_qp(true)
                        .use_max_qp(true)
                        .min_qp(vk::VideoEncodeH264QpEXT {
                            qp_i: settings.min_qp as i32,
                            qp_p: settings.min_qp as i32,
                            qp_b: settings.min_qp as i32,
                        })
                        .max_qp(vk::VideoEncodeH264QpEXT {
                            qp_i: settings.max_qp as i32,
                            qp_p: settings.max_qp as i32,
                            qp_b: settings.max_qp as i32,
                        }),
                );
            }

            // We can't do this in one step because the borrow checker doesn't
            // like the way push_next borrows.
            // TODO: Ash 0.39 may make this easier.
            for (layer, (settings, h264)) in layer_settings
                .iter()
                .zip(h264_rc_layers.iter_mut())
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
                        .push_next(h264),
                );
            }
        }

        let mut h264_rc_info = vk::VideoEncodeH264RateControlInfoEXT::default()
            .gop_frame_count(self.structure.gop_size)
            .idr_period(self.structure.gop_size)
            .consecutive_b_frame_count(0)
            .temporal_layer_count(rc_layers.len() as u32)
            .flags(vk::VideoEncodeH264RateControlFlagsEXT::REGULAR_GOP | pattern);

        let vbv_size = match self.rc_mode {
            RateControlMode::Vbr(vbr) => vbr.vbv_size_ms,
            _ => 0,
        };

        let mut rc_info = vk::VideoEncodeRateControlInfoKHR::default()
            .rate_control_mode(self.rc_mode.as_vk_flags())
            .virtual_buffer_size_in_ms(vbv_size)
            .layers(&rc_layers);

        // Doesn't have a push_next method, because we're supposed to call it on the
        // parent struct.
        rc_info.p_next = <*mut _>::cast(&mut h264_rc_info);

        let weight_table: vk::native::StdVideoEncodeH264WeightTable = std::mem::zeroed();

        let slice_type = if frame_state.is_keyframe {
            vk::native::StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_I
        } else {
            vk::native::StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_P
        };

        let primary_pic_type = if frame_state.is_keyframe {
            vk::native::StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR
        } else {
            vk::native::StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_P
        };

        let mut std_slice_header = vk::native::StdVideoEncodeH264SliceHeader {
            slice_type,
            pWeightTable: &weight_table,
            ..std::mem::zeroed()
        };

        // Per the spec, this indicates that all slices in the picture are the same.
        std_slice_header.slice_type += 5;

        let nalu_slice_entries = [vk::VideoEncodeH264NaluSliceInfoEXT::default()
            .std_slice_header(&std_slice_header)
            .constant_qp(if let RateControlMode::ConstantQp(qp) = self.rc_mode {
                qp.layer(frame_state.id) as i32
            } else {
                0
            })];

        let list0_mod_ops = std::mem::zeroed();
        let list1_mod_ops = std::mem::zeroed();
        let marking_ops = std::mem::zeroed();

        let mut ref_lists_info = vk::native::StdVideoEncodeH264ReferenceListsInfo {
            pRefList0ModOperations: &list0_mod_ops,
            pRefList1ModOperations: &list1_mod_ops,
            pRefPicMarkingOperations: &marking_ops,
            RefPicList0: [u8::MAX; 32],
            RefPicList1: [u8::MAX; 32],
            ..std::mem::zeroed()
        };

        // Point to the references.
        for (idx, id) in frame_state.ref_ids.iter().enumerate() {
            let slot = self
                .inner
                .dpb
                .get_pic(*id)
                .ok_or(anyhow::anyhow!("ref pic {id} missing from dpb",))?;
            ref_lists_info.RefPicList0[idx] = slot.index as u8;
        }

        let mut std_pic_info = vk::native::StdVideoEncodeH264PictureInfo {
            flags: std::mem::zeroed(),
            seq_parameter_set_id: 0,
            pic_parameter_set_id: 0,
            idr_pic_id: self.idr_num as u16,
            primary_pic_type,
            frame_num: self.frame_num,
            PicOrderCnt: frame_state.gop_position as i32,
            temporal_id: frame_state.id as u8,
            pRefLists: &ref_lists_info,
            ..std::mem::zeroed()
        };

        std_pic_info
            .flags
            .set_IdrPicFlag(frame_state.is_keyframe as u32);
        std_pic_info
            .flags
            .set_is_reference((frame_state.forward_ref_count > 0) as u32);

        let mut h264_pic_info = vk::VideoEncodeH264PictureInfoEXT::default()
            .nalu_slice_entries(&nalu_slice_entries)
            .std_picture_info(&std_pic_info);

        let mut std_ref_infos = frame_state
            .ref_ids
            .iter()
            .map(|id| vk::native::StdVideoEncodeH264ReferenceInfo {
                FrameNum: self.pic_metadata[*id as usize].frame_num,
                PicOrderCnt: self.pic_metadata[*id as usize].pic_order_cnt,
                temporal_id: *id as u8,
                ..std::mem::zeroed()
            })
            .collect::<Vec<_>>();

        let mut ref_info = std_ref_infos
            .iter_mut()
            .map(|info| vk::VideoEncodeH264DpbSlotInfoEXT::default().std_reference_info(info))
            .collect::<Vec<_>>();

        let setup_std_ref_info = vk::native::StdVideoEncodeH264ReferenceInfo {
            FrameNum: self.frame_num,
            PicOrderCnt: frame_state.gop_position as i32,
            temporal_id: frame_state.id as u8,
            ..std::mem::zeroed()
        };

        trace!(
            frame_num = self.frame_num,
            pic_order_cnt = frame_state.gop_position,
            "setting up h264 pic"
        );

        let mut setup_info =
            vk::VideoEncodeH264DpbSlotInfoEXT::default().std_reference_info(&setup_std_ref_info);

        let insert = if frame_state.is_keyframe {
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
            &mut h264_pic_info,
            &mut setup_info,
            &mut ref_info,
            insert,
        )?;

        // Save the reference info for the DPB slot we just wrote.
        self.pic_metadata[frame_state.id as usize] = H264Metadata {
            frame_num: self.frame_num,
            pic_order_cnt: frame_state.gop_position as i32,
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

    pub fn request_refresh(&mut self) {
        self.structure.request_refresh()
    }
}
