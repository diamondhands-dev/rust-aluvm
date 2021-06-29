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

use core::convert::TryInto;
use core::fmt::{self, Debug, Display, Formatter};

use amplify_num::{u1, u2, u24, u3, u4, u5, u6, u7};

use super::{Read, Write};
use crate::reg::{Number, RegisterSet};

// I had an idea of putting Read/Write functionality into `amplify` crate,
// but it is quire specific to the fact that it uses `u16`-sized underlying
// bytestring, which is specific to client-side-validation and this VM and not
// generic enough to become part of the `amplify` library

/// Errors with cursor-based operations
#[derive(Clone, Copy, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display)]
#[display(doc_comments)]
#[cfg_attr(feature = "std", derive(Error))]
pub enum CursorError {
    /// Attempt to read or write after end of data
    Eof,

    /// Attempt to read or write at a position outside of data boundaries ({0})
    OutOfBoundaries(usize),
}

/// Cursor for accessing byte string data bounded by `u16::MAX` length
pub struct Cursor<T, D>
where
    T: AsRef<[u8]>,
    D: AsRef<[u8]>,
{
    bytecode: T,
    byte_pos: u16,
    bit_pos: u3,
    eof: bool,
    data: D,
}

#[cfg(feature = "std")]
impl<T, D> Debug for Cursor<T, D>
where
    T: AsRef<[u8]>,
    D: AsRef<[u8]>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        use amplify_num::hex::ToHex;
        f.debug_struct("Cursor")
            .field("bytecode", &self.as_ref().to_hex())
            .field("byte_pos", &self.byte_pos)
            .field("bit_pos", &self.bit_pos)
            .field("eof", &self.eof)
            .field("data", &self.data.as_ref().to_hex())
            .finish()
    }
}

#[cfg(feature = "std")]
impl<T, D> Display for Cursor<T, D>
where
    T: AsRef<[u8]>,
    D: AsRef<[u8]>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        use amplify_num::hex::ToHex;
        write!(f, "{}:{} @ ", self.byte_pos, self.bit_pos)?;
        let hex = self.as_ref().to_hex();
        if f.alternate() {
            write!(f, "{}..{}", &hex[..4], &hex[hex.len() - 4..])?;
        } else {
            f.write_str(&hex)?;
        }
        let hex = self.data.as_ref().to_hex();
        if f.alternate() {
            write!(f, "{}..{}", &hex[..4], &hex[hex.len() - 4..])
        } else {
            f.write_str(&hex)
        }
    }
}

impl<T, D> Cursor<T, D>
where
    T: AsRef<[u8]>,
    D: AsRef<[u8]>,
{
    /// Creates cursor from the provided byte string
    ///
    /// # Panics
    ///
    /// If the length of the bytecode exceeds `u16::MAX` or length of the data `u24::MAX`
    #[inline]
    pub fn with(bytecode: T, data: D) -> Cursor<T, D> {
        assert!(bytecode.as_ref().len() <= u16::MAX as usize + 1);
        assert!(data.as_ref().len() <= u24::MAX.as_u32() as usize + 1);
        Cursor { bytecode, byte_pos: 0, bit_pos: u3::MIN, eof: false, data }
    }

    /// Returns whether cursor is at the upper length boundary for any byte
    /// string (equal to `u16::MAX`)
    #[inline]
    pub fn is_eof(&self) -> bool { self.eof }

    /// Returns current byte offset of the cursor. Does not accounts bits.
    #[inline]
    pub fn pos(&self) -> u16 { self.byte_pos }

    /// Sets current cursor byte offset to the provided value
    #[inline]
    pub fn seek(&mut self, byte_pos: u16) { self.byte_pos = byte_pos; }

    /// Converts writer into data accumulated from the instructions (i.e. data segment)
    #[inline]
    pub fn into_data(self) -> D { self.data }

    #[inline]
    fn as_ref(&self) -> &[u8] { self.bytecode.as_ref() }

    fn extract(&mut self, bit_count: u3) -> Result<u8, CursorError> {
        if self.eof {
            return Err(CursorError::Eof);
        }
        let byte = self.as_ref()[self.byte_pos as usize];
        let mut mask = 0x00u8;
        let mut cnt = bit_count.as_u8();
        while cnt > 0 {
            mask <<= 1;
            mask |= 0x01;
            cnt -= 1;
        }
        mask <<= self.bit_pos.as_u8();
        let val = (byte & mask) >> self.bit_pos.as_u8();
        self.inc_bits(bit_count).map(|_| val)
    }

    fn inc_bits(&mut self, bit_count: u3) -> Result<(), CursorError> {
        if self.eof {
            return Err(CursorError::Eof);
        }
        let pos = self.bit_pos.as_u8() + bit_count.as_u8();
        self.bit_pos = u3::with(pos % 8);
        self._inc_bytes_inner(pos as u16 / 8)
    }

    fn inc_bytes(&mut self, byte_count: u16) -> Result<(), CursorError> {
        assert_eq!(
            self.bit_pos.as_u8(),
            0,
            "attempt to access (multiple) bytes at a non-byte aligned position"
        );
        if self.eof {
            return Err(CursorError::Eof);
        }
        self._inc_bytes_inner(byte_count)
    }

    fn _inc_bytes_inner(&mut self, byte_count: u16) -> Result<(), CursorError> {
        if byte_count == 1 && self.byte_pos == u16::MAX {
            self.eof = true
        } else {
            self.byte_pos = self.byte_pos.checked_add(byte_count).ok_or(
                CursorError::OutOfBoundaries(self.byte_pos as usize + byte_count as usize),
            )?;
        }
        Ok(())
    }
}

impl<T, D> Cursor<T, D>
where
    T: AsRef<[u8]> + AsMut<[u8]>,
    D: AsRef<[u8]>,
{
    fn as_mut(&mut self) -> &mut [u8] { self.bytecode.as_mut() }
}

impl<T> Cursor<T, Vec<u8>>
where
    T: AsRef<[u8]> + AsMut<[u8]>,
{
    fn write_unique(&mut self, bytes: &[u8]) -> Result<u24, CursorError> {
        // We write the value only if the value is not yet present in the data segment
        let len = bytes.len();
        let offset = self.data.len();
        if let Some(offset) = self.data.windows(len).position(|window| window == bytes) {
            Ok(u24::with(offset as u32))
        } else if offset + len > u24::MAX.as_u32() as usize + 1 {
            Err(CursorError::OutOfBoundaries(offset + len))
        } else {
            self.data.extend(bytes);
            Ok(u24::with(offset as u32))
        }
    }
}

impl<T, D> Read for Cursor<T, D>
where
    T: AsRef<[u8]>,
    D: AsRef<[u8]>,
{
    type Error = CursorError;

    fn is_end(&self) -> bool { self.byte_pos as usize >= self.as_ref().len() }

    fn peek_u8(&self) -> Result<u8, CursorError> {
        if self.eof {
            return Err(CursorError::Eof);
        }
        Ok(self.as_ref()[self.byte_pos as usize])
    }

    fn read_bool(&mut self) -> Result<bool, CursorError> {
        if self.eof {
            return Err(CursorError::Eof);
        }
        let byte = self.extract(u3::with(1))?;
        Ok(byte == 0x01)
    }

    fn read_u1(&mut self) -> Result<u1, Self::Error> {
        Ok(self.extract(u3::with(1))?.try_into().expect("bit extractor failure"))
    }

    fn read_u2(&mut self) -> Result<u2, CursorError> {
        Ok(self.extract(u3::with(2))?.try_into().expect("bit extractor failure"))
    }

    fn read_u3(&mut self) -> Result<u3, CursorError> {
        Ok(self.extract(u3::with(3))?.try_into().expect("bit extractor failure"))
    }

    fn read_u4(&mut self) -> Result<u4, CursorError> {
        Ok(self.extract(u3::with(4))?.try_into().expect("bit extractor failure"))
    }

    fn read_u5(&mut self) -> Result<u5, CursorError> {
        Ok(self.extract(u3::with(5))?.try_into().expect("bit extractor failure"))
    }

    fn read_u6(&mut self) -> Result<u6, CursorError> {
        Ok(self.extract(u3::with(6))?.try_into().expect("bit extractor failure"))
    }

    fn read_u7(&mut self) -> Result<u7, CursorError> {
        Ok(self.extract(u3::with(7))?.try_into().expect("bit extractor failure"))
    }

    fn read_u8(&mut self) -> Result<u8, CursorError> {
        if self.eof {
            return Err(CursorError::Eof);
        }
        let byte = self.as_ref()[self.byte_pos as usize];
        self.inc_bytes(1).map(|_| byte)
    }

    fn read_u16(&mut self) -> Result<u16, CursorError> {
        if self.eof {
            return Err(CursorError::Eof);
        }
        let pos = self.byte_pos as usize;
        let mut buf = [0u8; 2];
        buf.copy_from_slice(&self.as_ref()[pos..pos + 2]);
        let word = u16::from_le_bytes(buf);
        self.inc_bytes(2).map(|_| word)
    }

    fn read_i16(&mut self) -> Result<i16, Self::Error> {
        if self.eof {
            return Err(CursorError::Eof);
        }
        let pos = self.byte_pos as usize;
        let mut buf = [0u8; 2];
        buf.copy_from_slice(&self.as_ref()[pos..pos + 2]);
        let word = i16::from_le_bytes(buf);
        self.inc_bytes(2).map(|_| word)
    }

    fn read_u24(&mut self) -> Result<u24, CursorError> {
        if self.eof {
            return Err(CursorError::Eof);
        }
        let pos = self.byte_pos as usize;
        let mut buf = [0u8; 3];
        buf.copy_from_slice(&self.as_ref()[pos..pos + 3]);
        let word = u24::from_le_bytes(buf);
        self.inc_bytes(3).map(|_| word)
    }

    fn read_bytes32(&mut self) -> Result<[u8; 32], CursorError> {
        if self.eof {
            return Err(CursorError::Eof);
        }
        let pos = self.byte_pos as usize;
        let mut buf = [0u8; 32];
        buf.copy_from_slice(&self.as_ref()[pos..pos + 32]);
        self.inc_bytes(32).map(|_| buf)
    }

    fn read_data(&mut self) -> Result<(&[u8], bool), CursorError> {
        let offset = self.read_u24()?.as_u32() as usize;
        let end = offset + self.read_u16()? as usize;
        let max = u24::MAX.as_u32() as usize;
        let st0 = if end > self.data.as_ref().len() { true } else { false };
        let data = &self.data.as_ref()[offset.min(max)..end.min(max)];
        Ok((data, st0))
    }

    fn read_number(&mut self, reg: impl RegisterSet) -> Result<Number, CursorError> {
        let offset = self.read_u24()?.as_u32() as usize;
        let end = offset + reg.bytes() as usize;
        if end > self.data.as_ref().len() {
            return Err(CursorError::Eof);
        }
        Ok(Number::from_slice(&self.data.as_ref()[offset..end]))
    }
}

impl<T> Write for Cursor<T, Vec<u8>>
where
    T: AsRef<[u8]> + AsMut<[u8]>,
{
    type Error = CursorError;

    fn write_bool(&mut self, data: bool) -> Result<(), CursorError> {
        let data = if data { 1u8 } else { 0u8 } << self.bit_pos.as_u8();
        let pos = self.byte_pos as usize;
        self.as_mut()[pos] |= data;
        self.inc_bits(u3::with(1))
    }

    fn write_u1(&mut self, data: impl Into<u1>) -> Result<(), Self::Error> {
        let data = data.into().as_u8() << self.bit_pos.as_u8();
        let pos = self.byte_pos as usize;
        self.as_mut()[pos] |= data;
        self.inc_bits(u3::with(1))
    }

    fn write_u2(&mut self, data: impl Into<u2>) -> Result<(), CursorError> {
        let data = data.into().as_u8() << self.bit_pos.as_u8();
        let pos = self.byte_pos as usize;
        self.as_mut()[pos] |= data;
        self.inc_bits(u3::with(2))
    }

    fn write_u3(&mut self, data: impl Into<u3>) -> Result<(), CursorError> {
        let data = data.into().as_u8() << self.bit_pos.as_u8();
        let pos = self.byte_pos as usize;
        self.as_mut()[pos] |= data;
        self.inc_bits(u3::with(3))
    }

    fn write_u4(&mut self, data: impl Into<u4>) -> Result<(), CursorError> {
        let data = data.into().as_u8() << self.bit_pos.as_u8();
        let pos = self.byte_pos as usize;
        self.as_mut()[pos] |= data;
        self.inc_bits(u3::with(4))
    }

    fn write_u5(&mut self, data: impl Into<u5>) -> Result<(), CursorError> {
        let data = data.into().as_u8() << self.bit_pos.as_u8();
        let pos = self.byte_pos as usize;
        self.as_mut()[pos] |= data;
        self.inc_bits(u3::with(5))
    }

    fn write_u6(&mut self, data: impl Into<u6>) -> Result<(), CursorError> {
        let data = data.into().as_u8() << self.bit_pos.as_u8();
        let pos = self.byte_pos as usize;
        self.as_mut()[pos] |= data;
        self.inc_bits(u3::with(6))
    }

    fn write_u7(&mut self, data: impl Into<u7>) -> Result<(), CursorError> {
        let data = data.into().as_u8() << self.bit_pos.as_u8();
        let pos = self.byte_pos as usize;
        self.as_mut()[pos] |= data;
        self.inc_bits(u3::with(7))
    }

    fn write_u8(&mut self, data: impl Into<u8>) -> Result<(), CursorError> {
        let pos = self.byte_pos as usize;
        self.as_mut()[pos] = data.into();
        self.inc_bytes(1)
    }

    fn write_u16(&mut self, data: impl Into<u16>) -> Result<(), CursorError> {
        let data = data.into().to_le_bytes();
        let pos = self.byte_pos as usize;
        self.as_mut()[pos] = data[0];
        self.as_mut()[pos + 1] = data[1];
        self.inc_bytes(2)
    }

    fn write_i16(&mut self, data: impl Into<i16>) -> Result<(), Self::Error> {
        let data = data.into().to_le_bytes();
        let pos = self.byte_pos as usize;
        self.as_mut()[pos] = data[0];
        self.as_mut()[pos + 1] = data[1];
        self.inc_bytes(2)
    }

    fn write_u24(&mut self, data: impl Into<u24>) -> Result<(), CursorError> {
        let data = data.into().to_le_bytes();
        let pos = self.byte_pos as usize;
        self.as_mut()[pos] = data[0];
        self.as_mut()[pos + 1] = data[1];
        self.as_mut()[pos + 2] = data[2];
        self.inc_bytes(3)
    }

    fn write_bytes32(&mut self, data: [u8; 32]) -> Result<(), CursorError> {
        let from = self.byte_pos as usize;
        let to = from + 32;
        self.as_mut()[from..to].copy_from_slice(&data);
        self.inc_bytes(32)
    }

    fn write_data(&mut self, bytes: impl AsRef<[u8]>) -> Result<(), CursorError> {
        // We control that `self.byte_pos + bytes.len() < u16` at buffer
        // allocation time, so if we panic here this means we have a bug in
        // out allocation code and has to kill the process and report this issue
        let bytes = bytes.as_ref();
        let len = bytes.len();
        if len >= u16::MAX as usize {
            return Err(CursorError::OutOfBoundaries(len));
        }
        let offset = self.write_unique(bytes)?;
        self.write_u24(offset)?;
        self.write_u16(len as u16)
    }

    fn write_number(
        &mut self,
        reg: impl RegisterSet,
        mut value: Number,
    ) -> Result<(), CursorError> {
        let len = reg.bytes();
        assert!(
            len <= value.len(),
            "value for the register has larger bit length than the register"
        );
        value.reshape(reg.layout().using_sign(value.layout()));
        let offset = self.write_unique(&value[..])?;
        self.write_u24(offset)
    }
}
