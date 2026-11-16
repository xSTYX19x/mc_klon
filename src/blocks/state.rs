//! Block states: a block together with its data byte, read and written through
//! typed views onto the bits each block declares in its schema
//! (`BlockSpec::properties`, set in `blocks!`).
//!
//! Storage stays a `BlockId` plus one byte per voxel (`Chunk::block_data`,
//! saved as is by the v2 column format). `BlockState` only carries the two
//! together through the API, so the storage can change later without touching
//! the callers.

use std::fmt;
use std::marker::PhantomData;

use super::{BlockId, CollisionShape, OcclusionShape, block_key_str_for_block, block_spec_for};

/// A block and its data byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BlockState {
    pub(crate) id: BlockId,
    pub(crate) data: u8,
}

impl BlockState {
    pub(crate) const fn new(id: BlockId, data: u8) -> Self {
        Self { id, data }
    }

    /// Every property at its default. Defaults are raw value 0, so this is
    /// data 0 (enforced by `every_schema_fits_its_data_byte`).
    pub(crate) const fn default_of(id: BlockId) -> Self {
        Self::new(id, 0)
    }

    /// The block's schema: which bits of `data` mean what.
    pub(crate) fn properties(self) -> &'static [PropertySpec] {
        block_spec_for(self.id).properties
    }

    pub(crate) fn collision_shape(self) -> CollisionShape {
        block_spec_for(self.id).collision_shape()
    }

    pub(crate) fn raycast_shape(self) -> CollisionShape {
        block_spec_for(self.id).raycast_shape()
    }

    /// Visibility/AO occlusion shape. This is intentionally separate from
    /// collision, raycast selection, and render shape.
    pub(crate) fn occlusion_shape(self) -> OcclusionShape {
        block_spec_for(self.id).occlusion_shape()
    }
}

impl From<BlockId> for BlockState {
    fn from(id: BlockId) -> Self {
        Self::default_of(id)
    }
}

/// `minecraft:oak_sapling[stage=1]`: the block key, then the properties in
/// schema order; blocks without properties print the key alone.
impl fmt::Display for BlockState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(block_key_str_for_block(self.id))?;
        let properties = self.properties();
        if properties.is_empty() {
            return Ok(());
        }
        f.write_str("[")?;
        for (index, property) in properties.iter().enumerate() {
            if index > 0 {
                f.write_str(",")?;
            }
            write!(f, "{}=", property.name)?;
            property.fmt_value(property.raw(self.data), f)?;
        }
        f.write_str("]")
    }
}

/// How a property's raw bits read as a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PropertyKind {
    Bool,
    /// A number `min..=max`, stored as `value - min`.
    Int {
        min: u8,
        max: u8,
    },
    /// One of the names, stored as its index.
    Enum(&'static [&'static str]),
}

/// One property's slice of the data byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PropertySpec {
    pub(crate) name: &'static str,
    pub(crate) shift: u8,
    pub(crate) bits: u8,
    pub(crate) kind: PropertyKind,
}

impl PropertySpec {
    pub(crate) const fn mask(self) -> u8 {
        (((1_u16 << self.bits) - 1) << self.shift) as u8
    }

    pub(crate) const fn raw(self, data: u8) -> u8 {
        (data & self.mask()) >> self.shift
    }

    pub(crate) const fn with_raw(self, data: u8, raw: u8) -> u8 {
        (data & !self.mask()) | ((raw << self.shift) & self.mask())
    }

    /// How many raw values mean something; raw values `0..value_count`.
    #[cfg(test)]
    pub(crate) const fn value_count(self) -> u16 {
        match self.kind {
            PropertyKind::Bool => 2,
            PropertyKind::Int { min, max } => max as u16 - min as u16 + 1,
            PropertyKind::Enum(names) => names.len() as u16,
        }
    }

    fn fmt_value(self, raw: u8, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            PropertyKind::Bool => write!(f, "{}", raw != 0),
            PropertyKind::Int { min, .. } => write!(f, "{}", raw as u16 + min as u16),
            PropertyKind::Enum(names) => match names.get(raw as usize) {
                Some(name) => f.write_str(name),
                None => write!(f, "?{raw}"),
            },
        }
    }
}

/// A value type a property can be read as.
pub(crate) trait PropertyValue: Copy {
    fn from_raw(raw: u8, kind: PropertyKind) -> Self;
    fn to_raw(self, kind: PropertyKind) -> u8;
}

impl PropertyValue for bool {
    fn from_raw(raw: u8, _kind: PropertyKind) -> Self {
        raw != 0
    }

    fn to_raw(self, _kind: PropertyKind) -> u8 {
        self as u8
    }
}

impl PropertyValue for u8 {
    fn from_raw(raw: u8, kind: PropertyKind) -> Self {
        match kind {
            PropertyKind::Int { min, .. } => raw + min,
            _ => raw,
        }
    }

    fn to_raw(self, kind: PropertyKind) -> u8 {
        match kind {
            PropertyKind::Int { min, max } => {
                debug_assert!((min..=max).contains(&self), "{self} outside {min}..={max}");
                self - min
            }
            _ => self,
        }
    }
}

/// An enum read by its index in `NAMES`.
pub(crate) trait EnumProperty: Copy + 'static {
    const NAMES: &'static [&'static str];
    const BITS: u8;
    fn from_index(index: u8) -> Self;
    fn index(self) -> u8;
}

impl<T: EnumProperty> PropertyValue for T {
    fn from_raw(raw: u8, _kind: PropertyKind) -> Self {
        T::from_index(raw)
    }

    fn to_raw(self, _kind: PropertyKind) -> u8 {
        self.index()
    }
}

/// A typed view onto one property: `get` and `with` on a `BlockState`, and
/// the same on a bare data byte for code that holds the two apart
/// (`world::fluids`).
pub(crate) struct Property<T> {
    spec: PropertySpec,
    value: PhantomData<T>,
}

impl<T> Clone for Property<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Property<T> {}

impl<T> Property<T> {
    const fn from_spec(spec: PropertySpec) -> Self {
        Self {
            spec,
            value: PhantomData,
        }
    }

    pub(crate) const fn spec(self) -> PropertySpec {
        self.spec
    }
}

impl Property<bool> {
    pub(crate) const fn flag(name: &'static str, shift: u8) -> Self {
        Self::from_spec(PropertySpec {
            name,
            shift,
            bits: 1,
            kind: PropertyKind::Bool,
        })
    }
}

impl Property<u8> {
    /// A number `min..=max` in `bits` bits, stored as `value - min`.
    pub(crate) const fn int(name: &'static str, shift: u8, bits: u8, min: u8, max: u8) -> Self {
        Self::from_spec(PropertySpec {
            name,
            shift,
            bits,
            kind: PropertyKind::Int { min, max },
        })
    }
}

impl<T: EnumProperty> Property<T> {
    pub(crate) const fn enumeration(name: &'static str, shift: u8) -> Self {
        Self::from_spec(PropertySpec {
            name,
            shift,
            bits: T::BITS,
            kind: PropertyKind::Enum(T::NAMES),
        })
    }
}

impl<T: PropertyValue> Property<T> {
    pub(crate) fn get(self, state: BlockState) -> T {
        self.get_data(state.data)
    }

    pub(crate) fn with(self, state: BlockState, value: T) -> BlockState {
        BlockState::new(state.id, self.with_data(state.data, value))
    }

    pub(crate) fn get_data(self, data: u8) -> T {
        T::from_raw(self.spec.raw(data), self.spec.kind)
    }

    pub(crate) fn with_data(self, data: u8, value: T) -> u8 {
        self.spec.with_raw(data, value.to_raw(self.spec.kind))
    }
}

// --- Schemas of today's blocks -------------------------------------------------

/// A sapling's growth stage: stage 1 grows into a tree on the next successful
/// random tick.
pub(crate) const SAPLING_STAGE: Property<u8> = Property::int("stage", 0, 1, 0, 1);
pub(crate) const SAPLING_PROPERTIES: [PropertySpec; 1] = [SAPLING_STAGE.spec()];

/// Flow distance from the source; the fluid simulation uses 0..=7.
pub(crate) const FLUID_LEVEL: Property<u8> = Property::int("level", 0, 4, 0, 15);
/// Generated water and lava that stays dormant until something touches it.
pub(crate) const FLUID_STATIC: Property<bool> = Property::flag("static", 5);
/// A source block (placed or generated) rather than flowing fluid.
pub(crate) const FLUID_SOURCE: Property<bool> = Property::flag("source", 6);
pub(crate) const FLUID_FALLING: Property<bool> = Property::flag("falling", 7);
pub(crate) const FLUID_PROPERTIES: [PropertySpec; 4] = [
    FLUID_LEVEL.spec(),
    FLUID_FALLING.spec(),
    FLUID_SOURCE.spec(),
    FLUID_STATIC.spec(),
];

/// Typed views for the block forms still to come (stairs, slabs, doors,
/// rails, redstone, crops, snow layers). Each form family picks the bit
/// offsets for its blocks and declares them in `blocks!`; until the first one
/// lands, only the tests use these.
#[allow(dead_code)]
pub(crate) mod views {
    use super::{EnumProperty, Property};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Facing4 {
        North,
        East,
        South,
        West,
    }

    impl EnumProperty for Facing4 {
        const NAMES: &'static [&'static str] = &["north", "east", "south", "west"];
        const BITS: u8 = 2;
        fn from_index(index: u8) -> Self {
            [Self::North, Self::East, Self::South, Self::West][index as usize & 3]
        }
        fn index(self) -> u8 {
            self as u8
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Facing6 {
        Down,
        Up,
        North,
        South,
        West,
        East,
    }

    impl EnumProperty for Facing6 {
        const NAMES: &'static [&'static str] = &["down", "up", "north", "south", "west", "east"];
        const BITS: u8 = 3;
        fn from_index(index: u8) -> Self {
            const ALL: [Facing6; 6] = [
                Facing6::Down,
                Facing6::Up,
                Facing6::North,
                Facing6::South,
                Facing6::West,
                Facing6::East,
            ];
            ALL[(index as usize).min(5)]
        }
        fn index(self) -> u8 {
            self as u8
        }
    }

    /// Slab, stair and door halves.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Half {
        Bottom,
        Top,
    }

    impl EnumProperty for Half {
        const NAMES: &'static [&'static str] = &["bottom", "top"];
        const BITS: u8 = 1;
        fn from_index(index: u8) -> Self {
            if index & 1 == 0 {
                Self::Bottom
            } else {
                Self::Top
            }
        }
        fn index(self) -> u8 {
            self as u8
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum RailShape {
        NorthSouth,
        EastWest,
        AscendingEast,
        AscendingWest,
        AscendingNorth,
        AscendingSouth,
        SouthEast,
        SouthWest,
        NorthWest,
        NorthEast,
    }

    impl EnumProperty for RailShape {
        const NAMES: &'static [&'static str] = &[
            "north_south",
            "east_west",
            "ascending_east",
            "ascending_west",
            "ascending_north",
            "ascending_south",
            "south_east",
            "south_west",
            "north_west",
            "north_east",
        ];
        const BITS: u8 = 4;
        fn from_index(index: u8) -> Self {
            const ALL: [RailShape; 10] = [
                RailShape::NorthSouth,
                RailShape::EastWest,
                RailShape::AscendingEast,
                RailShape::AscendingWest,
                RailShape::AscendingNorth,
                RailShape::AscendingSouth,
                RailShape::SouthEast,
                RailShape::SouthWest,
                RailShape::NorthWest,
                RailShape::NorthEast,
            ];
            ALL[(index as usize).min(9)]
        }
        fn index(self) -> u8 {
            self as u8
        }
    }

    pub(crate) const fn facing4(shift: u8) -> Property<Facing4> {
        Property::enumeration("facing", shift)
    }

    pub(crate) const fn facing6(shift: u8) -> Property<Facing6> {
        Property::enumeration("facing", shift)
    }

    pub(crate) const fn half(shift: u8) -> Property<Half> {
        Property::enumeration("half", shift)
    }

    pub(crate) const fn open(shift: u8) -> Property<bool> {
        Property::flag("open", shift)
    }

    pub(crate) const fn powered(shift: u8) -> Property<bool> {
        Property::flag("powered", shift)
    }

    pub(crate) const fn rail_shape(shift: u8) -> Property<RailShape> {
        Property::enumeration("shape", shift)
    }

    pub(crate) const fn redstone_power(shift: u8) -> Property<u8> {
        Property::int("power", shift, 4, 0, 15)
    }

    /// Crop, cactus and sugar cane age `0..=max` (at most 15).
    pub(crate) const fn age(shift: u8, max: u8) -> Property<u8> {
        let bits = (u8::BITS - max.leading_zeros()) as u8;
        Property::int("age", shift, if bits == 0 { 1 } else { bits }, 0, max)
    }

    /// Snow layers `1..=8`.
    pub(crate) const fn snow_layers(shift: u8) -> Property<u8> {
        Property::int("layers", shift, 3, 1, 8)
    }
}

#[cfg(test)]
mod tests {
    use super::views::{
        Facing4, Facing6, Half, RailShape, age, facing4, facing6, half, open, powered, rail_shape,
        redstone_power, snow_layers,
    };
    use super::*;
    use crate::blocks::BLOCK_SPECS;

    /// The schema contract: per block, the properties fit the byte, do not
    /// overlap, can hold every value, have distinct names, and read raw 0 as a
    /// valid default (so `default_of` is data 0).
    #[test]
    fn every_schema_fits_its_data_byte() {
        for spec in BLOCK_SPECS {
            let mut used = 0_u8;
            for (index, property) in spec.properties.iter().enumerate() {
                assert!(
                    property.bits >= 1 && property.shift + property.bits <= 8,
                    "{}: {} leaves the data byte",
                    spec.key,
                    property.name
                );
                assert_eq!(
                    used & property.mask(),
                    0,
                    "{}: {} overlaps another property",
                    spec.key,
                    property.name
                );
                used |= property.mask();
                let count = property.value_count();
                assert!(
                    count >= 1 && count <= 1 << property.bits,
                    "{}: {} has {count} values for {} bits",
                    spec.key,
                    property.name,
                    property.bits
                );
                assert!(
                    spec.properties[..index]
                        .iter()
                        .all(|other| other.name != property.name),
                    "{}: {} declared twice",
                    spec.key,
                    property.name
                );
            }
        }
    }

    /// Fluid blocks own exactly the bits the fluid simulation uses, and the
    /// views read and write them as the old helpers did.
    #[test]
    fn fluid_blocks_declare_exactly_the_fluid_bits() {
        for block in [
            BlockId::WaterStill,
            BlockId::WaterFlowing,
            BlockId::LavaStill,
            BlockId::LavaFlowing,
        ] {
            let properties = BlockState::default_of(block).properties();
            assert_eq!(properties, FLUID_PROPERTIES, "{block:?}");
            let mask = properties.iter().fold(0, |m, p| m | p.mask());
            assert_eq!(mask, 0x0F | 0x20 | 0x40 | 0x80, "{block:?}");
        }
        for data in 0..=u8::MAX {
            assert_eq!(FLUID_LEVEL.get_data(data), data & 0x0F);
            assert_eq!(FLUID_STATIC.get_data(data), data & 0x20 != 0);
            assert_eq!(FLUID_SOURCE.get_data(data), data & 0x40 != 0);
            assert_eq!(FLUID_FALLING.get_data(data), data & 0x80 != 0);
        }
    }

    #[test]
    fn sapling_stage_is_bit_zero() {
        let state = BlockState::default_of(BlockId::Sapling);
        assert_eq!(state.properties(), SAPLING_PROPERTIES);
        assert_eq!(SAPLING_STAGE.get(state), 0);
        let grown = SAPLING_STAGE.with(state, 1);
        assert_eq!(grown.data, 1);
        assert_eq!(SAPLING_STAGE.get(grown), 1);
        // Bits outside the stage are left alone.
        assert_eq!(SAPLING_STAGE.with_data(0xF0, 1), 0xF1);
    }

    #[test]
    fn states_print_as_key_and_properties() {
        assert_eq!(
            BlockState::from(BlockId::Stone).to_string(),
            "minecraft:stone"
        );
        let sapling = SAPLING_STAGE.with(BlockId::Sapling.into(), 1);
        assert_eq!(sapling.to_string(), "minecraft:oak_sapling[stage=1]");
        let source = FLUID_SOURCE.with(BlockId::WaterStill.into(), true);
        assert_eq!(
            source.to_string(),
            "minecraft:water[level=0,falling=false,source=true,static=false]"
        );
    }

    #[test]
    fn views_round_trip_through_the_byte() {
        let base = BlockState::new(BlockId::Stone, 0);
        for facing in [Facing4::North, Facing4::East, Facing4::South, Facing4::West] {
            let view = facing4(2);
            assert_eq!(view.get(view.with(base, facing)), facing);
        }
        let view = facing6(1);
        for facing in [Facing6::Down, Facing6::Up, Facing6::West, Facing6::East] {
            assert_eq!(view.get(view.with(base, facing)), facing);
        }
        assert_eq!(half(7).get(half(7).with(base, Half::Top)), Half::Top);
        assert!(open(3).get(open(3).with(base, true)));
        assert!(!powered(4).get(base));
        let shape = rail_shape(0);
        assert_eq!(
            shape.get(shape.with(base, RailShape::NorthEast)),
            RailShape::NorthEast
        );
        assert_eq!(redstone_power(4).get(redstone_power(4).with(base, 15)), 15);
        assert_eq!(age(0, 15).spec().bits, 4);
        assert_eq!(age(0, 7).spec().bits, 3);
        assert_eq!(age(0, 1).spec().bits, 1);
        let layers = snow_layers(0);
        assert_eq!(layers.get(base), 1, "raw 0 is one layer");
        assert_eq!(layers.with(base, 8).data, 7);
    }
}
