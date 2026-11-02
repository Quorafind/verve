//! The shape a clone has in memory: a tree of plain Rust values, nothing a realm owns.

/// A value copied out of a realm. Objects are numbered in the order they were first met, and
/// met again they are [`Tree::Seen`] — cycles and shared parts come out as they went in.
#[derive(Clone, Debug)]
pub(crate) enum Tree {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    /// UTF-16, as JavaScript keeps it: a lone surrogate crosses as it is.
    String(Vec<u16>),
    BigInt(String),
    Date(f64),
    RegExp {
        source: Vec<u16>,
        flags: Vec<u16>,
    },
    /// An `ArrayBuffer`'s bytes.
    ArrayBuffer(Vec<u8>),
    /// A typed array or a `DataView`: what it showed, and the kind it was.
    View {
        kind: ViewKind,
        bytes: Vec<u8>,
    },
    /// A `Blob`, or a `File` when it has a name.
    Blob {
        bytes: Vec<u8>,
        media_type: String,
        name: Option<String>,
        modified: Option<f64>,
    },
    Array(Vec<Tree>),
    Object(Vec<(Vec<u16>, Tree)>),
    /// An error: its own properties, with its name and message among them.
    Error(Vec<(Vec<u16>, Tree)>),
    Map(Vec<(Tree, Tree)>),
    Set(Vec<Tree>),
    Seen(usize),
}

/// Which of JavaScript's views a [`Tree::View`] came from. The discriminants are the numbers
/// the byte format writes, so they are fixed here and [`ViewKind::ALL`] reads them back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ViewKind {
    Int8 = 0,
    Uint8 = 1,
    Uint8Clamped = 2,
    Int16 = 3,
    Uint16 = 4,
    Int32 = 5,
    Uint32 = 6,
    Float32 = 7,
    Float64 = 8,
    BigInt64 = 9,
    BigUint64 = 10,
    DataView = 11,
}

impl ViewKind {
    /// Every kind, in the order of its discriminant.
    const ALL: [Self; 12] = [
        Self::Int8,
        Self::Uint8,
        Self::Uint8Clamped,
        Self::Int16,
        Self::Uint16,
        Self::Int32,
        Self::Uint32,
        Self::Float32,
        Self::Float64,
        Self::BigInt64,
        Self::BigUint64,
        Self::DataView,
    ];

    /// The kind `kind` is, when it is one that crosses.
    pub(crate) fn of(kind: boa_engine::builtins::typed_array::TypedArrayKind) -> Option<Self> {
        use boa_engine::builtins::typed_array::TypedArrayKind as K;
        Some(match kind {
            K::Int8 => Self::Int8,
            K::Uint8 => Self::Uint8,
            K::Uint8Clamped => Self::Uint8Clamped,
            K::Int16 => Self::Int16,
            K::Uint16 => Self::Uint16,
            K::Int32 => Self::Int32,
            K::Uint32 => Self::Uint32,
            K::BigInt64 => Self::BigInt64,
            K::BigUint64 => Self::BigUint64,
            K::Float32 => Self::Float32,
            K::Float64 => Self::Float64,
            // Boa's `float16` feature (on by default, so a dependent crate can turn it on for
            // everyone) adds `Float16`, which this format has no number for.
            #[allow(unreachable_patterns)]
            _ => return None,
        })
    }

    /// The number the byte format writes for this kind.
    pub(crate) fn number(self) -> u8 {
        self as u8
    }

    /// The kind written as `number`, if there is one.
    pub(crate) fn from_number(number: u8) -> Option<Self> {
        Self::ALL.get(usize::from(number)).copied()
    }

    /// The typed array kind Boa makes for this one; `None` for a `DataView`, which is not one.
    pub(crate) fn boa(self) -> Option<boa_engine::builtins::typed_array::TypedArrayKind> {
        use boa_engine::builtins::typed_array::TypedArrayKind as K;
        Some(match self {
            Self::Int8 => K::Int8,
            Self::Uint8 => K::Uint8,
            Self::Uint8Clamped => K::Uint8Clamped,
            Self::Int16 => K::Int16,
            Self::Uint16 => K::Uint16,
            Self::Int32 => K::Int32,
            Self::Uint32 => K::Uint32,
            Self::Float32 => K::Float32,
            Self::Float64 => K::Float64,
            Self::BigInt64 => K::BigInt64,
            Self::BigUint64 => K::BigUint64,
            Self::DataView => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ViewKind;

    #[test]
    fn every_kind_reads_back_as_the_number_it_is_written_as() {
        for (at, kind) in ViewKind::ALL.iter().enumerate() {
            assert_eq!(usize::from(kind.number()), at);
            assert_eq!(ViewKind::from_number(kind.number()), Some(*kind));
        }
        assert_eq!(ViewKind::from_number(12), None);
    }
}
