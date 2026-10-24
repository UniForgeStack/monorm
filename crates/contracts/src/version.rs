//! `major.minor` contract versions. A minor release only adds; a major release can break.
use std::fmt;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContractVersion { pub major: u16, pub minor: u16 }

impl ContractVersion {
    pub const fn new(major: u16, minor: u16) -> Self { Self { major, minor } }

    /// A library of this version runs on `engine`: equal majors and a minor that the engine has.
    /// A major of 0 can break on a minor change, and the rule still compares both parts.
    pub fn fits(self, engine: Self) -> bool { self.major == engine.major && self.minor <= engine.minor }

    /// `what` names the contract in the error: `schema contract`, `ABI`.
    pub fn check(self, what: &str, engine: Self, id: &str) -> Result<(), String> {
        if self.fits(engine) { return Ok(()); }
        if self.major != engine.major {
            return Err(format!("{id}: {what} {self} does not fit this engine ({what} {engine}): the major versions differ; install a release of {id} built for {what} {}.x, or use an ORM release that has {what} {}.x", engine.major, self.major));
        }
        Err(format!("{id}: {what} {self} is newer than this engine ({what} {engine}); update the ORM, or install an older release of {id}"))
    }

    /// The u32 of the C ABI: the major in the high 16 bits, the minor in the low 16 bits.
    pub const fn to_bits(self) -> u32 { ((self.major as u32) << 16) | self.minor as u32 }
    pub const fn from_bits(bits: u32) -> Self { Self { major: (bits >> 16) as u16, minor: (bits & 0xffff) as u16 } }
}

impl fmt::Display for ContractVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}.{}", self.major, self.minor) }
}

impl std::str::FromStr for ContractVersion {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, String> {
        let bad = || format!("invalid contract version {text:?}: write it as \"major.minor\", for example \"0.1\"");
        let (major, minor) = text.split_once('.').map_or((text, "0"), |(a, b)| (a, b));
        Ok(Self { major: major.parse().map_err(|_| bad())?, minor: minor.parse().map_err(|_| bad())? })
    }
}

impl Serialize for ContractVersion {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> { s.collect_str(self) }
}

impl<'de> Deserialize<'de> for ContractVersion {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl de::Visitor<'_> for Visitor {
            type Value = ContractVersion;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("an integer major or a \"major.minor\" string") }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                u16::try_from(v).map(|major| ContractVersion { major, minor: 0 }).map_err(|_| E::custom("contract major out of range"))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                u64::try_from(v).map_err(|_| E::custom("contract major out of range")).and_then(|v| self.visit_u64(v))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Err(E::custom(format!("a contract version is not a number ({v}): write it as a string, for example \"{v:.1}\"")))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> { v.parse().map_err(E::custom) }
        }
        d.deserialize_any(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_reads_an_integer_and_a_string_and_writes_a_string() {
        let v = |s: &str| -> Result<ContractVersion, String> { serde_json::from_str(s).map_err(|e| e.to_string()) };
        assert_eq!(v("2"), Ok(ContractVersion::new(2, 0)));
        assert_eq!(v("\"0.1\""), Ok(ContractVersion::new(0, 1)));
        assert_eq!(v("\"3\""), Ok(ContractVersion::new(3, 0)));
        assert_eq!(serde_json::to_string(&ContractVersion::new(2, 0)).unwrap(), "\"2.0\"");
        assert!(v("\"a.b\"").unwrap_err().contains("major.minor"));
        assert!(v("-1").is_err());
    }

    #[test]
    fn it_rejects_a_toml_float() {
        #[derive(Deserialize, Debug)]
        struct T { _v: ContractVersion }
        assert_eq!(toml::from_str::<T>("_v = 2").unwrap()._v, ContractVersion::new(2, 0));
        assert_eq!(toml::from_str::<T>("_v = \"0.1\"").unwrap()._v, ContractVersion::new(0, 1));
        let error = toml::from_str::<T>("_v = 0.1").unwrap_err().to_string();
        assert!(error.contains("write it as a string"), "{error}");
    }

    #[test]
    fn it_fits_when_majors_are_equal_and_the_minor_is_not_higher() {
        let engine = ContractVersion::new(2, 1);
        assert!(ContractVersion::new(2, 0).fits(engine) && ContractVersion::new(2, 1).fits(engine));
        assert!(!ContractVersion::new(2, 2).fits(engine) && !ContractVersion::new(1, 0).fits(engine) && !ContractVersion::new(3, 0).fits(engine));
        assert!(!ContractVersion::new(0, 2).fits(ContractVersion::new(0, 1)));
    }

    #[test]
    fn the_errors_name_both_versions_and_the_extension() {
        let engine = ContractVersion::new(2, 0);
        let major = ContractVersion::new(3, 0).check("schema contract", engine, "acme.x").unwrap_err();
        assert!(major.contains("acme.x") && major.contains("3.0") && major.contains("2.0") && major.contains("major"), "{major}");
        let minor = ContractVersion::new(2, 4).check("schema contract", engine, "acme.x").unwrap_err();
        assert!(minor.contains("acme.x") && minor.contains("2.4") && minor.contains("2.0") && minor.contains("update the ORM"), "{minor}");
    }

    #[test]
    fn the_abi_bits_keep_both_parts() {
        let v = ContractVersion::new(0, 1);
        assert_eq!(v.to_bits(), 1);
        assert_eq!(ContractVersion::from_bits(ContractVersion::new(3, 7).to_bits()), ContractVersion::new(3, 7));
    }
}
