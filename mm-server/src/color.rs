/// A combination of color primaries, white point, and transfer function. We
/// generally ignore white point, since we deal only with colorspaces using the
/// D65 white point.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ColorSpace {
    /// Uses BT.709 primaries and the sRGB transfer function.
    Srgb,
    /// Uses BT.709 primaries and a linear transfer function. Usually encoded as
    /// a float with negative values and values above 1.0 used to represent the
    /// extended space.
    LinearExtSrgb,
    /// Uses BT.2020 primaries and the ST2084 (PQ) transfer function.
    Hdr10,
}

impl ColorSpace {
    pub fn from_primaries_and_tf(
        primaries: Primaries,
        transfer_function: TransferFunction,
    ) -> Option<Self> {
        match (primaries, transfer_function) {
            (Primaries::Srgb, TransferFunction::Srgb) => Some(ColorSpace::Srgb),
            (Primaries::Srgb, TransferFunction::Linear) => Some(ColorSpace::LinearExtSrgb),
            (Primaries::Bt2020, TransferFunction::Pq) => Some(ColorSpace::Hdr10),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum TransferFunction {
    Linear,
    Srgb,
    Pq,
}

#[derive(Debug, Clone, Copy)]
pub enum Primaries {
    Srgb,
    Bt2020,
}
