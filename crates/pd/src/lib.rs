#![no_std]

//! Bounded USB PD messages and FUSB302 FIFO framing.
//!
//! Wire fields follow the USB Power Delivery specification and the
//! [FUSB302B datasheet, tables 41–42](https://www.onsemi.com/pdf/datasheet/fusb302b-d.pdf).
//! [`Link`] owns message counters; callers own retries and power transitions.

mod apple;
mod link;
pub mod power;
mod source;

pub use apple::{APPLE_REBOOT, APPLE_SILICON_DFU, APPLE_T2_DFU};
pub use link::Link;
pub use source::{FixedRequest, RequestError, fixed_5v, validate_fixed_5v_request};

pub const MAX_OBJECTS: usize = 7;
/// SOP tokens, PACKSYM, header, seven objects and four trailing TX commands.
pub const MAX_TX_BYTES: usize = 39;
/// RX token, header, seven objects and four CRC bytes.
pub const MAX_RX_BYTES: usize = 35;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    ExtendedMessage,
    ReservedRevision,
    InvalidHeaderField,
    ObjectCount,
    BufferTooSmall,
    RxLength,
    RxToken,
    Crc,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Revision {
    Rev10,
    Rev20,
    Rev30,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PowerRole {
    Sink,
    Source,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataRole {
    Ufp,
    Dfp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header(u16);

impl Header {
    pub fn parse(bits: u16) -> Result<Self, Error> {
        if bits & 0x8000 != 0 {
            return Err(Error::ExtendedMessage);
        }
        if (bits >> 6) & 3 == 3 {
            return Err(Error::ReservedRevision);
        }
        Ok(Self(bits))
    }

    /// Build a PD 2.0 source/DFP header without wrapping an invalid field.
    pub fn source(message_type: u8, id: u8, object_count: u8) -> Result<Self, Error> {
        if message_type > 15 || id > 7 || object_count > MAX_OBJECTS as u8 {
            return Err(Error::InvalidHeaderField);
        }
        Ok(Self(
            u16::from(message_type)
                | 1 << 5
                | 1 << 6
                | 1 << 8
                | u16::from(id) << 9
                | u16::from(object_count) << 12,
        ))
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn message_type(self) -> u8 {
        (self.0 & 0x1f) as u8
    }

    pub const fn id(self) -> u8 {
        ((self.0 >> 9) & 7) as u8
    }

    pub const fn object_count(self) -> u8 {
        ((self.0 >> 12) & 7) as u8
    }

    pub const fn power_role(self) -> PowerRole {
        if self.0 & (1 << 8) == 0 {
            PowerRole::Sink
        } else {
            PowerRole::Source
        }
    }

    pub const fn data_role(self) -> DataRole {
        if self.0 & (1 << 5) == 0 {
            DataRole::Ufp
        } else {
            DataRole::Dfp
        }
    }

    pub const fn revision(self) -> Revision {
        match (self.0 >> 6) & 3 {
            0 => Revision::Rev10,
            1 => Revision::Rev20,
            _ => Revision::Rev30,
        }
    }

    /// Total RX FIFO packet size, including its token and four CRC bytes.
    pub const fn rx_len(self) -> usize {
        7 + self.object_count() as usize * 4
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sop {
    Sop,
    Prime,
    PrimePrime,
    DebugPrime,
    DebugPrimePrime,
}

impl Sop {
    fn tx_tokens(self) -> [u8; 4] {
        match self {
            Self::Sop => [0x12, 0x12, 0x12, 0x13],
            Self::Prime => [0x12, 0x12, 0x1b, 0x1b],
            Self::PrimePrime => [0x12, 0x1b, 0x12, 0x1b],
            Self::DebugPrime => [0x12, 0x16, 0x16, 0x1b],
            Self::DebugPrimePrime => [0x12, 0x16, 0x1b, 0x13],
        }
    }

    fn from_rx_token(token: u8) -> Result<Self, Error> {
        match token & 0xe0 {
            0xe0 => Ok(Self::Sop),
            0xc0 => Ok(Self::Prime),
            0xa0 => Ok(Self::PrimePrime),
            0x80 => Ok(Self::DebugPrime),
            0x60 => Ok(Self::DebugPrimePrime),
            _ => Err(Error::RxToken),
        }
    }
}

/// Encode FIFO tokens, without the I²C register-address byte (`0x43`).
/// The FUSB302 generates the CRC and starts transmission on the final TXON.
pub fn encode_tx<'a>(
    sop: Sop,
    header: Header,
    objects: &[u32],
    output: &'a mut [u8],
) -> Result<&'a [u8], Error> {
    if objects.len() != usize::from(header.object_count()) || objects.len() > MAX_OBJECTS {
        return Err(Error::ObjectCount);
    }
    let payload_len = 2 + 4 * objects.len();
    let length = 9 + payload_len;
    if output.len() < length {
        return Err(Error::BufferTooSmall);
    }
    output[..4].copy_from_slice(&sop.tx_tokens());
    output[4] = 0x80 | payload_len as u8;
    output[5..7].copy_from_slice(&header.bits().to_le_bytes());
    for (object, bytes) in objects
        .iter()
        .zip(output[7..length - 4].as_chunks_mut::<4>().0)
    {
        bytes.copy_from_slice(&object.to_le_bytes());
    }
    output[length - 4..length].copy_from_slice(&[0xff, 0x14, 0xfe, 0xa1]);
    Ok(&output[..length])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceivedPacket {
    pub sop: Sop,
    pub header: Header,
    objects: [u32; MAX_OBJECTS],
    /// Received CRC bytes, verified against the header and data objects.
    pub crc: [u8; 4],
}

impl ReceivedPacket {
    pub fn objects(&self) -> &[u32] {
        &self.objects[..usize::from(self.header.object_count())]
    }
}

/// Decode exactly one FUSB302 RX packet and verify its four trailing CRC bytes.
pub fn decode_rx(bytes: &[u8]) -> Result<ReceivedPacket, Error> {
    if bytes.len() < 3 || bytes.len() > MAX_RX_BYTES {
        return Err(Error::RxLength);
    }
    let sop = Sop::from_rx_token(bytes[0])?;
    let header = Header::parse(u16::from_le_bytes([bytes[1], bytes[2]]))?;
    if bytes.len() != header.rx_len() {
        return Err(Error::RxLength);
    }
    let crc_start = bytes.len() - 4;
    let mut crc = [0; 4];
    crc.copy_from_slice(&bytes[crc_start..]);
    if crc32(&bytes[1..crc_start]).to_le_bytes() != crc {
        return Err(Error::Crc);
    }
    let mut objects = [0; MAX_OBJECTS];
    for (object, bytes) in objects
        .iter_mut()
        .zip(bytes[3..crc_start].as_chunks::<4>().0)
    {
        *object = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }
    Ok(ReceivedPacket {
        sop,
        header,
        objects,
        crc,
    })
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_header_has_pd20_roles_and_preserves_message_fields() {
        let header = Header::source(15, 5, 3).unwrap();
        assert_eq!(header.bits(), 0x3b6f);
        assert_eq!(header.message_type(), 15);
        assert_eq!(header.id(), 5);
        assert_eq!(header.object_count(), 3);
        assert_eq!(header.power_role(), PowerRole::Source);
        assert_eq!(header.data_role(), DataRole::Dfp);
        assert_eq!(header.revision(), Revision::Rev20);
        assert_eq!(Header::parse(header.bits()), Ok(header));

        let sink = Header::parse(0x0082).unwrap();
        assert_eq!(sink.power_role(), PowerRole::Sink);
        assert_eq!(sink.data_role(), DataRole::Ufp);
        assert_eq!(sink.revision(), Revision::Rev30);
        for fields in [(16, 0, 0), (1, 8, 0), (1, 0, 8)] {
            assert_eq!(
                Header::source(fields.0, fields.1, fields.2),
                Err(Error::InvalidHeaderField)
            );
        }
        assert_eq!(Header::parse(0x8001), Err(Error::ExtendedMessage));
        assert_eq!(Header::parse(0x00c1), Err(Error::ReservedRevision));
    }

    #[test]
    fn tx_fifo_bytes_match_source_capabilities_and_apple_debug_messages() {
        let mut output = [0; MAX_TX_BYTES];
        assert_eq!(
            encode_tx(
                Sop::Sop,
                Header::source(1, 0, 1).unwrap(),
                &[fixed_5v(500, true)],
                &mut output
            )
            .unwrap(),
            &[
                0x12, 0x12, 0x12, 0x13, 0x86, 0x61, 0x11, 0x32, 0x90, 0x01, 0x0c, 0xff, 0x14, 0xfe,
                0xa1
            ]
        );
        assert_eq!(
            encode_tx(
                Sop::DebugPrimePrime,
                Header::source(15, 5, 3).unwrap(),
                &APPLE_SILICON_DFU,
                &mut output
            )
            .unwrap(),
            &[
                0x12, 0x16, 0x1b, 0x13, 0x8e, 0x6f, 0x3b, 0x12, 0x80, 0xac, 0x05, 0x06, 0x01, 0x00,
                0x00, 0x00, 0x00, 0x01, 0x80, 0xff, 0x14, 0xfe, 0xa1
            ]
        );
    }

    #[test]
    fn tx_rejects_inconsistent_counts_and_never_truncates() {
        let mut output = [0xa5; MAX_TX_BYTES];
        let header = Header::source(1, 7, 7).unwrap();
        let objects = [u32::MAX; MAX_OBJECTS];
        let packet = encode_tx(Sop::Sop, header, &objects, &mut output).unwrap();
        assert_eq!(packet.len(), MAX_TX_BYTES);
        assert_eq!(packet[4], 0x9e);
        assert_eq!(
            encode_tx(Sop::Sop, header, &objects[..6], &mut output),
            Err(Error::ObjectCount)
        );
        assert_eq!(
            encode_tx(Sop::Sop, header, &[0; 8], &mut output),
            Err(Error::ObjectCount)
        );
        assert_eq!(
            encode_tx(Sop::Sop, header, &objects, &mut output[..38]),
            Err(Error::BufferTooSmall)
        );
        let control =
            encode_tx(Sop::Sop, Header::source(3, 1, 0).unwrap(), &[], &mut output).unwrap();
        assert_eq!(control.len(), 11);
        assert_eq!(control[4], 0x82);
    }

    #[test]
    fn rx_decodes_objects_and_consumes_crc_as_separate_bytes() {
        let bytes = [
            0xff, 0x42, 0x10, 0x32, 0xc8, 0x00, 0x10, 0xf0, 0x7a, 0x9b, 0x4b,
        ];
        let packet = decode_rx(&bytes).unwrap();
        assert_eq!(packet.sop, Sop::Sop);
        assert_eq!(packet.header.message_type(), 2);
        assert_eq!(packet.header.revision(), Revision::Rev20);
        assert_eq!(packet.objects(), &[0x1000_c832]);
        assert_eq!(packet.crc, [0xf0, 0x7a, 0x9b, 0x4b]);
        for length in 0..bytes.len() {
            assert!(decode_rx(&bytes[..length]).is_err());
        }
        let mut excess = [0; 12];
        excess[..bytes.len()].copy_from_slice(&bytes);
        assert_eq!(decode_rx(&excess), Err(Error::RxLength));
        assert_eq!(decode_rx(&[0; MAX_RX_BYTES + 1]), Err(Error::RxLength));
    }

    #[test]
    fn rx_distinguishes_sop_tokens_and_rejects_unsupported_headers() {
        for (token, sop) in [
            (0xff, Sop::Sop),
            (0xdf, Sop::Prime),
            (0xbf, Sop::PrimePrime),
            (0x9f, Sop::DebugPrime),
            (0x7f, Sop::DebugPrimePrime),
        ] {
            let packet = decode_rx(&[token, 1, 0, 0xbe, 0x23, 0xc2, 0x58]).unwrap();
            assert_eq!(packet.sop, sop);
            assert!(packet.objects().is_empty());
        }
        assert_eq!(decode_rx(&[0x5f, 1, 0, 0, 0, 0, 0]), Err(Error::RxToken));
        assert_eq!(
            decode_rx(&[0xe0, 1, 0x80, 0, 0, 0, 0]),
            Err(Error::ExtendedMessage)
        );
        assert_eq!(
            decode_rx(&[0xe0, 0xc1, 0, 0, 0, 0, 0]),
            Err(Error::ReservedRevision)
        );
    }

    #[test]
    fn rx_accepts_all_seven_objects_without_including_crc_in_the_payload() {
        let mut bytes = [0xff; MAX_RX_BYTES];
        bytes[..3].copy_from_slice(&[0xe0, 0x41, 0x70]);
        bytes[MAX_RX_BYTES - 4..].copy_from_slice(&[0x99, 0xd0, 0xaf, 0x02]);
        let packet = decode_rx(&bytes).unwrap();
        assert_eq!(packet.objects(), &[u32::MAX; MAX_OBJECTS]);
        assert_eq!(packet.crc, [0x99, 0xd0, 0xaf, 0x02]);
    }

    #[test]
    fn rx_rejects_corruption_in_the_header_objects_or_crc() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        let golden = [
            0xe0, 0x42, 0x10, 0x32, 0xc8, 0x00, 0x10, 0xf0, 0x7a, 0x9b, 0x4b,
        ];
        assert!(decode_rx(&golden).is_ok());
        for byte in [1, 3, 4, 5, 6, 7, 8, 9, 10] {
            for bit in 0..8 {
                let mut corrupted = golden;
                corrupted[byte] ^= 1 << bit;
                assert!(decode_rx(&corrupted).is_err());
            }
        }
        let mut corrupt_header = golden;
        corrupt_header[1] ^= 1;
        assert_eq!(decode_rx(&corrupt_header), Err(Error::Crc));
        let mut corrupt_object = golden;
        corrupt_object[3] ^= 1;
        assert_eq!(decode_rx(&corrupt_object), Err(Error::Crc));
        let mut corrupt_crc = golden;
        corrupt_crc[10] ^= 1;
        assert_eq!(decode_rx(&corrupt_crc), Err(Error::Crc));
    }
}
