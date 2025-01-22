// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

#[derive(Debug, Clone, PartialEq, Eq)]

pub struct GopFrame {
    pub stream_position: u64,
    pub gop_position: u64,

    pub id: u32,
    /// The frame IDs this frame references.
    pub ref_ids: Vec<u32>,
    pub is_keyframe: bool,
    /// The number of frames referencing this one.
    pub forward_ref_count: u32,
}

/// This implements hierarchical P-coding, which looks like this:
/// https://eymenkurdoglu.github.io/2016/07/01/hierp-one.html
///
/// This is also called a "dyadic" structure by the Vulkan spec (42.17.11. H.264
/// Encode Rate Control).
///
/// Each frame references at most one other frame. The pattern repeats every
/// (2^(layers-1)) frames, but an intra frame is only used once per GOP. Note
/// that a 1-layer structure is equivalent to a flat P structure, with each
/// frame referencing the one before.
pub struct HierarchicalP {
    pub layers: u32,
    pub gop_size: u32,
    pub mini_gop_size: u32,
    frame_num: u64,
}

impl HierarchicalP {
    pub fn new(layers: u32, gop_size: u32) -> Self {
        assert!(layers > 0);
        assert!(layers <= 5);

        let mini_gop_size = 2_u32.pow(layers - 1);
        assert_eq!(gop_size % mini_gop_size, 0);

        Self {
            layers,
            gop_size,
            mini_gop_size,
            frame_num: 0,
        }
    }

    pub fn next_frame(&mut self) -> GopFrame {
        let mini_gop_pos = (self.frame_num % self.mini_gop_size as u64) as u32;
        let (layer, ref_layer) = if mini_gop_pos == 0 {
            (0, 0)
        } else {
            let ref_pos = mini_gop_pos ^ (1 << mini_gop_pos.trailing_zeros());

            (
                temporal_layer(mini_gop_pos, self.layers),
                temporal_layer(ref_pos, self.layers),
            )
        };

        let gop_position = self.frame_num % self.gop_size as u64;
        let ref_ids = if gop_position == 0 {
            vec![]
        } else {
            vec![ref_layer]
        };

        let forward_ref_count = if layer == 0 {
            // One for each layer above, plus the next mini-GOP.
            self.layers
        } else {
            // One for each layer above.
            self.layers - layer - 1
        };

        // We use the layer as the frame ID.
        let frame = GopFrame {
            stream_position: self.frame_num,
            gop_position,

            id: layer,
            ref_ids,
            is_keyframe: gop_position == 0,
            forward_ref_count,
        };

        self.frame_num += 1;
        frame
    }

    pub fn required_dpb_size(&self) -> usize {
        // We should have one slot for each layer.
        std::cmp::max(self.layers as usize, 2)
    }

    /// Returns the number of frames per second belonging to a particular layer
    /// as a fractional number, given the layer and the total framerate.
    pub fn layer_framerate(&self, layer: u32, base_framerate: u32) -> (u32, u32) {
        if self.layers == 1 {
            return (base_framerate, 1);
        }

        let frames_per_mini_gop = 2_u32.pow(layer.saturating_sub(1)); // 1, 1, 2, 4, 8, 16...
        assert!(frames_per_mini_gop <= self.mini_gop_size / 2);

        (base_framerate * frames_per_mini_gop, self.mini_gop_size)
    }
}

fn temporal_layer(frame: u32, layers: u32) -> u32 {
    if frame == 0 {
        return 0;
    }

    layers - frame.trailing_zeros() - 1
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_temporal_layer_4_layers() {
        assert_eq!(temporal_layer(0, 4), 0);
        assert_eq!(temporal_layer(1, 4), 3);
        assert_eq!(temporal_layer(2, 4), 2);
        assert_eq!(temporal_layer(3, 4), 3);
        assert_eq!(temporal_layer(4, 4), 1);
        assert_eq!(temporal_layer(5, 4), 3);
        assert_eq!(temporal_layer(6, 4), 2);
        assert_eq!(temporal_layer(7, 4), 3);
    }

    #[test]
    fn test_gop() {
        let mut structure = HierarchicalP::new(3, 60);
        assert_eq!(structure.gop_size, 60);
        assert_eq!(structure.mini_gop_size, 4);

        let expected = [
            GopFrame {
                stream_position: 0,
                gop_position: 0,
                id: 0,
                ref_ids: vec![],
                is_keyframe: true,
                forward_ref_count: 3,
            },
            GopFrame {
                stream_position: 1,
                gop_position: 1,
                id: 2,
                ref_ids: vec![0],
                is_keyframe: false,
                forward_ref_count: 0,
            },
            GopFrame {
                stream_position: 2,
                gop_position: 2,
                id: 1,
                ref_ids: vec![0],
                is_keyframe: false,
                forward_ref_count: 1,
            },
            GopFrame {
                stream_position: 3,
                gop_position: 3,
                id: 2,
                ref_ids: vec![1],
                is_keyframe: false,
                forward_ref_count: 0,
            },
            GopFrame {
                stream_position: 4,
                gop_position: 4,
                id: 0,
                ref_ids: vec![0],
                is_keyframe: false,
                forward_ref_count: 3,
            },
            GopFrame {
                stream_position: 5,
                gop_position: 5,
                id: 2,
                ref_ids: vec![0],
                is_keyframe: false,
                forward_ref_count: 0,
            },
            GopFrame {
                stream_position: 6,
                gop_position: 6,
                id: 1,
                ref_ids: vec![0],
                is_keyframe: false,
                forward_ref_count: 1,
            },
            GopFrame {
                stream_position: 7,
                gop_position: 7,
                id: 2,
                ref_ids: vec![1],
                is_keyframe: false,
                forward_ref_count: 0,
            },
        ];

        for (i, frame) in expected.iter().enumerate() {
            assert_eq!(structure.next_frame(), *frame, "Frame {}", i);
        }
    }

    #[test]
    fn test_flat() {
        let mut structure = HierarchicalP::new(1, 60);

        let expected = [
            GopFrame {
                stream_position: 0,
                gop_position: 0,
                id: 0,
                ref_ids: vec![],
                is_keyframe: true,
                forward_ref_count: 1,
            },
            GopFrame {
                stream_position: 1,
                gop_position: 1,
                id: 0,
                ref_ids: vec![0],
                is_keyframe: false,
                forward_ref_count: 1,
            },
            GopFrame {
                stream_position: 2,
                gop_position: 2,
                id: 0,
                ref_ids: vec![0],
                is_keyframe: false,
                forward_ref_count: 1,
            },
            GopFrame {
                stream_position: 3,
                gop_position: 3,
                id: 0,
                ref_ids: vec![0],
                is_keyframe: false,
                forward_ref_count: 1,
            },
        ];

        for (i, frame) in expected.iter().enumerate() {
            assert_eq!(structure.next_frame(), *frame, "Frame {}", i);
        }
    }
}
