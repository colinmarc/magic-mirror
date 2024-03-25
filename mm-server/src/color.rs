#[derive(Debug, Clone, Copy)]
pub enum ColorSpace {
    /// Uses sRGB primaries and the sRGB transfer function.
    Srgb = 1,
    /// Uses Display P3 primaries and the sRGB transfer function.
    DisplayP3 = 2,
    /// Uses BT.2020 primaries and the ST2084 (PQ) transfer function.
    Hdr10 = 3,
}

impl ColorSpace {
    pub fn from_primaries_and_tf(
        primaries: Primaries,
        transfer_function: TransferFunction,
    ) -> Option<Self> {
        match (primaries, transfer_function) {
            (Primaries::Srgb, TransferFunction::Srgb) => Some(ColorSpace::Srgb),
            (Primaries::DisplayP3, TransferFunction::Srgb) => Some(ColorSpace::DisplayP3),
            (Primaries::Bt2020, TransferFunction::Pq) => Some(ColorSpace::Hdr10),
            _ => None,
        }
    }

    pub fn primaries(&self) -> Primaries {
        match self {
            ColorSpace::Srgb => Primaries::Srgb,
            ColorSpace::DisplayP3 => Primaries::DisplayP3,
            ColorSpace::Hdr10 => Primaries::Bt2020,
        }
    }

    pub fn transfer_function(&self) -> TransferFunction {
        match self {
            ColorSpace::Srgb => TransferFunction::Srgb,
            ColorSpace::DisplayP3 => TransferFunction::Srgb,
            ColorSpace::Hdr10 => TransferFunction::Pq,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum TransferFunction {
    Linear = 0,
    Srgb = 1,
    Pq = 2,
}

#[derive(Debug, Clone, Copy)]
pub enum Primaries {
    Srgb = 1,
    DisplayP3 = 2,
    Bt2020 = 3,
}
