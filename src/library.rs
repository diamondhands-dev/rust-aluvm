// Reference rust implementation of AluVM (arithmetic logic unit virtual machine).
// To find more on AluVM please check <https://github.com/internet2-org/aluvm-spec>
//
// Designed & written in 2021 by
//     Dr. Maxim Orlovsky <orlovsky@pandoracore.com>
// This work is donated to LNP/BP Standards Association by Pandora Core AG
//
// This software is licensed under the terms of MIT License.
// You should have received a copy of the MIT License along with this software.
// If not, see <https://opensource.org/licenses/MIT>.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt::{self, Display, Formatter};
use core::marker::PhantomData;

use amplify_num::u24;
use bitcoin_hashes::Hash;

use crate::encoding::Read;
use crate::instr::serialize::{Bytecode, DecodeError, EncodeError};
use crate::instr::{ExecStep, NOp};
use crate::{ByteStr, Cursor, Instr, InstructionSet, Registers};

const LIB_HASH_MIDSTATE: [u8; 32] = [
    156, 224, 228, 230, 124, 17, 108, 57, 56, 179, 202, 242, 195, 15, 80, 137, 211, 243, 147, 108,
    71, 99, 110, 96, 125, 179, 62, 234, 221, 198, 240, 201,
];

sha256t_hash_newtype!(
    LibHash,
    LibHashTag,
    LIB_HASH_MIDSTATE,
    64,
    doc = "Library reference: a hash of the library code",
    false
);

/// Errors happening during library creation from bytecode & data
#[derive(Clone, Copy, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display, From)]
#[display(doc_comments)]
#[cfg_attr(feature = "std", derive(Error))]
pub enum Error {
    /// The size of the code segment exceeds 2^16
    CodeSegmentTooLarge(usize),

    /// The size of the data segment exceeds 2^24
    DataSegmentTooLarge(usize),
}

/// AluVM executable code library
#[derive(Debug, Default)]
pub struct Lib<E = NOp>
where
    E: InstructionSet,
{
    code_segment: ByteStr,
    data_segment: Box<[u8]>,
    instruction_set: PhantomData<E>,
}

impl<E> Display for Lib<E>
where
    E: InstructionSet,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("ISAE: ")?;
        f.write_str(&Instr::<E>::ids().into_iter().collect::<Vec<_>>().join("+"))?;
        f.write_str("\nCODE: ")?;
        Display::fmt(&self.code_segment, f)?;
        f.write_str("\nDATA: ")?;
        let data = ByteStr::with(&self.data_segment);
        Display::fmt(&data, f)
    }
}

impl<E> Lib<E>
where
    E: InstructionSet,
{
    /// Constructs library from bytecode and data
    pub fn with(bytecode: Vec<u8>, data: Vec<u8>) -> Result<Lib<E>, Error> {
        if bytecode.len() > u16::MAX as usize {
            return Err(Error::CodeSegmentTooLarge(bytecode.len()));
        }
        if data.len() > u24::MAX.as_u32() as usize {
            return Err(Error::DataSegmentTooLarge(data.len()));
        }
        Ok(Self {
            code_segment: ByteStr::with(bytecode),
            data_segment: Box::from(data),
            instruction_set: Default::default(),
        })
    }

    /// Assembles library from the provided instructions by encoding them into bytecode
    pub fn assemble<I>(code: I) -> Result<Lib<E>, EncodeError>
    where
        I: IntoIterator,
        <I as IntoIterator>::Item: InstructionSet,
    {
        let mut code_segment = ByteStr::default();
        let mut writer = Cursor::with(&mut code_segment.bytes[..], vec![]);
        for instr in code.into_iter() {
            instr.write(&mut writer)?;
        }
        let pos = writer.pos();
        let data = writer.into_data();
        code_segment.adjust_len(pos, false);

        Ok(Lib {
            code_segment,
            data_segment: Box::from(data),
            instruction_set: PhantomData::<E>::default(),
        })
    }

    /// Disassembles library into a set of instructions
    pub fn disassemble(&self) -> Result<Vec<Instr<E>>, DecodeError> {
        let mut code = vec![];
        let mut reader = Cursor::with(&self.code_segment, &*self.data_segment);
        while !reader.is_end() {
            code.push(Instr::read(&mut reader)?);
        }
        Ok(code)
    }

    /// Returns hash identifier [`LibHash`], representing the library in a unique way.
    ///
    /// Lib hash is computed as SHA256 tagged hash of the serialized library bytecode.
    pub fn lib_hash(&self) -> LibHash { LibHash::hash(&*self.code_segment.bytes) }

    /// Returns reference to code segment
    pub fn code_segment(&self) -> &[u8] { self.code_segment.as_ref() }

    /// Returns reference to data segment
    pub fn data_segment(&self) -> &[u8] { self.data_segment.as_ref() }

    /// Executes library code starting at entrypoint
    pub fn run(&self, entrypoint: u16, registers: &mut Registers) -> Option<LibSite> {
        let mut cursor = Cursor::with(&self.code_segment.bytes[..], &*self.data_segment);
        let lib_hash = self.lib_hash();
        cursor.seek(entrypoint);

        while !cursor.is_eof() {
            let instr = Instr::<E>::read(&mut cursor).ok()?;
            match instr.exec(registers, LibSite::with(cursor.pos(), lib_hash)) {
                ExecStep::Stop => return None,
                ExecStep::Next => continue,
                ExecStep::Jump(pos) => cursor.seek(pos),
                ExecStep::Call(site) => return Some(site),
            }
        }

        None
    }
}

/// Location within a library
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Display)]
#[display("{pos:#06X}@{lib}")]
pub struct LibSite {
    /// Library hash
    pub lib: LibHash,

    /// Offset from the beginning of the code, in bytes
    pub pos: u16,
}

impl LibSite {
    /// Constricts library site reference from a given position and library hash
    /// value
    pub fn with(pos: u16, lib: LibHash) -> LibSite { LibSite { lib, pos } }
}
