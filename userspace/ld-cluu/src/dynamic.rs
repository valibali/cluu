use alloc::vec::Vec;

pub const DT_NULL: u64 = 0;
pub const DT_HASH: u64 = 4;
pub const DT_STRTAB: u64 = 5;
pub const DT_SYMTAB: u64 = 6;
pub const DT_RELA: u64 = 7;
pub const DT_RELASZ: u64 = 8;
pub const DT_RELAENT: u64 = 9;
pub const DT_STRSZ: u64 = 10;
pub const DT_SYMENT: u64 = 11;
pub const DT_INIT: u64 = 12;
pub const DT_FINI: u64 = 13;
pub const DT_NEEDED: u64 = 1;
pub const DT_PLTREL: u64 = 20;
pub const DT_JMPREL: u64 = 23;
pub const DT_INIT_ARRAY: u64 = 25;
pub const DT_FINI_ARRAY: u64 = 26;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynTag {
    Null,
    Needed,
    StrTab,
    SymTab,
    Rela,
    RelaSz,
    StrSz,
    SymEnt,
    RelaEnt,
    Hash,
    PltRel,
    JmpRel,
    InitArray,
    FiniArray,
    Other(u64),
}

impl DynTag {
    pub fn from_raw(raw: u64) -> Self {
        match raw {
            DT_NULL => DynTag::Null,
            DT_NEEDED => DynTag::Needed,
            DT_STRTAB => DynTag::StrTab,
            DT_SYMTAB => DynTag::SymTab,
            DT_RELA => DynTag::Rela,
            DT_RELASZ => DynTag::RelaSz,
            DT_STRSZ => DynTag::StrSz,
            DT_SYMENT => DynTag::SymEnt,
            DT_RELAENT => DynTag::RelaEnt,
            DT_HASH => DynTag::Hash,
            DT_PLTREL => DynTag::PltRel,
            DT_JMPREL => DynTag::JmpRel,
            DT_INIT_ARRAY => DynTag::InitArray,
            DT_FINI_ARRAY => DynTag::FiniArray,
            other => DynTag::Other(other),
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DynEntry {
    pub tag: u64,
    pub val: u64,
}

#[derive(Debug, Clone, Default)]
pub struct DynamicInfo {
    pub strtab: u64,
    pub symtab: u64,
    pub strsz: u64,
    pub rela: u64,
    pub relasz: u64,
    pub relaent: u64,
    pub syment: u64,
    pub jmprel: u64,
    pub needed: Vec<u64>,
}

impl DynamicInfo {
    pub fn parse(dynamic: &[DynEntry]) -> Self {
        let mut info = Self::default();
        for entry in dynamic {
            match DynTag::from_raw(entry.tag) {
                DynTag::StrTab => info.strtab = entry.val,
                DynTag::SymTab => info.symtab = entry.val,
                DynTag::StrSz => info.strsz = entry.val,
                DynTag::Rela => info.rela = entry.val,
                DynTag::RelaSz => info.relasz = entry.val,
                DynTag::RelaEnt => info.relaent = entry.val,
                DynTag::SymEnt => info.syment = entry.val,
                DynTag::JmpRel => info.jmprel = entry.val,
                DynTag::Needed => info.needed.push(entry.val),
                _ => {}
            }
        }
        info
    }
}
