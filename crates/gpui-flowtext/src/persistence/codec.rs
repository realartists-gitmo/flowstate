#[hotpath::measure]
fn read_u8(cursor: &mut Cursor<&[u8]>) -> io::Result<u8> {
  let mut bytes = [0; 1];
  cursor.read_exact(&mut bytes)?;
  Ok(bytes[0])
}

#[hotpath::measure]
fn read_u16(cursor: &mut Cursor<&[u8]>) -> io::Result<u16> {
  let mut bytes = [0; 2];
  cursor.read_exact(&mut bytes)?;
  Ok(u16::from_le_bytes(bytes))
}

#[hotpath::measure]
fn read_u32(cursor: &mut Cursor<&[u8]>) -> io::Result<u32> {
  let mut bytes = [0; 4];
  cursor.read_exact(&mut bytes)?;
  Ok(u32::from_le_bytes(bytes))
}

#[hotpath::measure]
fn write_u32(bytes: &mut Vec<u8>, value: u32) {
  bytes.extend_from_slice(&value.to_le_bytes());
}

#[hotpath::measure]
fn read_u64(cursor: &mut Cursor<&[u8]>) -> io::Result<u64> {
  let mut bytes = [0; 8];
  cursor.read_exact(&mut bytes)?;
  Ok(u64::from_le_bytes(bytes))
}

#[hotpath::measure]
fn read_u128(cursor: &mut Cursor<&[u8]>) -> io::Result<u128> {
  let mut bytes = [0; 16];
  cursor.read_exact(&mut bytes)?;
  Ok(u128::from_le_bytes(bytes))
}

#[hotpath::measure]
fn write_u64(bytes: &mut Vec<u8>, value: u64) {
  bytes.extend_from_slice(&value.to_le_bytes());
}

#[hotpath::measure]
fn write_u128(bytes: &mut Vec<u8>, value: u128) {
  bytes.extend_from_slice(&value.to_le_bytes());
}

#[hotpath::measure]
fn read_len(cursor: &mut Cursor<&[u8]>, label: &'static str) -> io::Result<usize> {
  let raw = read_u64(cursor)?;
  usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{label} overflows usize")))
}

#[hotpath::measure]
fn read_bytes<'bytes>(cursor: &mut Cursor<&'bytes [u8]>, len: usize, label: &'static str) -> io::Result<&'bytes [u8]> {
  let start = usize::try_from(cursor.position())
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{label} cursor position overflows usize")))?;
  let end = start
    .checked_add(len)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("{label} length overflows usize")))?;
  if end > cursor.get_ref().len() {
    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("{label} is truncated")));
  }
  cursor.set_position(end as u64);
  Ok(&cursor.get_ref()[start..end])
}

#[hotpath::measure]
fn read_string(cursor: &mut Cursor<&[u8]>) -> io::Result<String> {
  let len = read_len(cursor, "native document string length")?;
  let bytes = read_bytes(cursor, len, "native document string")?;
  std::str::from_utf8(bytes)
    .map(std::borrow::ToOwned::to_owned)
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "native document string is not UTF-8"))
}

#[hotpath::measure]
fn write_string(bytes: &mut Vec<u8>, value: &str) {
  write_u64(bytes, value.len() as u64);
  bytes.extend_from_slice(value.as_bytes());
}

const fn encode_block_alignment(alignment: BlockAlignment) -> u8 {
  match alignment {
    BlockAlignment::Left => 0,
    BlockAlignment::Center => 1,
    BlockAlignment::Right => 2,
  }
}

#[hotpath::measure]
fn decode_block_alignment(value: u8) -> io::Result<BlockAlignment> {
  match value {
    0 => Ok(BlockAlignment::Left),
    1 => Ok(BlockAlignment::Center),
    2 => Ok(BlockAlignment::Right),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid block alignment")),
  }
}

const fn encode_paragraph_style(style: ParagraphStyle) -> u8 {
  match style {
    ParagraphStyle::Custom(0) => 0,
    ParagraphStyle::Custom(1) => 1,
    ParagraphStyle::Custom(2) => 2,
    ParagraphStyle::Custom(3) => 3,
    ParagraphStyle::Custom(4) => 4,
    ParagraphStyle::Normal => 5,
    ParagraphStyle::Custom(6) => 6,
    ParagraphStyle::Custom(slot) => 128 + (slot & 0x7f),
  }
}

#[hotpath::measure]
fn decode_paragraph_style(value: u8) -> io::Result<ParagraphStyle> {
  match value {
    0 => Ok(ParagraphStyle::Custom(0)),
    1 => Ok(ParagraphStyle::Custom(1)),
    2 => Ok(ParagraphStyle::Custom(2)),
    3 => Ok(ParagraphStyle::Custom(3)),
    4 => Ok(ParagraphStyle::Custom(4)),
    5 => Ok(ParagraphStyle::Normal),
    6 => Ok(ParagraphStyle::Custom(6)),
    128..=255 => Ok(ParagraphStyle::Custom(value - 128)),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid paragraph style")),
  }
}

const fn encode_section_kind(kind: SectionKind) -> u8 {
  match kind {
    SectionKind::Custom(slot) => slot & 0x7f,
  }
}

#[hotpath::measure]
fn decode_section_kind(value: u8) -> io::Result<SectionKind> {
  match value {
    0..=127 => Ok(SectionKind::Custom(value)),
    128..=255 => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid section kind")),
  }
}

#[hotpath::measure]
fn write_run_styles(bytes: &mut Vec<u8>, styles: RunStyles) {
  bytes.push(encode_run_semantic_style(styles.semantic));
  let mut flags = 0_u8;
  if styles.direct_underline {
    flags |= 1 << 0;
  }
  if styles.strikethrough {
    flags |= 1 << 1;
  }
  bytes.push(flags);
  bytes.push(encode_highlight_style(styles.highlight));
}

#[hotpath::measure]
fn read_run_styles(cursor: &mut Cursor<&[u8]>) -> io::Result<RunStyles> {
  let semantic = decode_run_semantic_style(read_u8(cursor)?)?;
  let flags = read_u8(cursor)?;
  if flags & !0b0000_0011 != 0 {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid run style flags"));
  }
  Ok(RunStyles {
    semantic,
    direct_underline: flags & (1 << 0) != 0,
    strikethrough: flags & (1 << 1) != 0,
    highlight: decode_highlight_style(read_u8(cursor)?)?,
  })
}

const fn encode_run_semantic_style(style: RunSemanticStyle) -> u8 {
  match style {
    RunSemanticStyle::Plain => 0,
    RunSemanticStyle::Custom(1) => 1,
    RunSemanticStyle::Custom(2) => 2,
    RunSemanticStyle::Custom(3) => 3,
    RunSemanticStyle::Custom(4) => 4,
    RunSemanticStyle::Custom(5) => 5,
    RunSemanticStyle::Custom(slot) => 128 + (slot & 0x7f),
  }
}

#[hotpath::measure]
fn decode_run_semantic_style(value: u8) -> io::Result<RunSemanticStyle> {
  match value {
    0 => Ok(RunSemanticStyle::Plain),
    1 => Ok(RunSemanticStyle::Custom(1)),
    2 => Ok(RunSemanticStyle::Custom(2)),
    3 => Ok(RunSemanticStyle::Custom(3)),
    4 => Ok(RunSemanticStyle::Custom(4)),
    5 => Ok(RunSemanticStyle::Custom(5)),
    128..=255 => Ok(RunSemanticStyle::Custom(value - 128)),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid run semantic style")),
  }
}

const fn encode_highlight_style(style: Option<HighlightStyle>) -> u8 {
  match style {
    None => 0,
    Some(HighlightStyle::Custom(1)) => 1,
    Some(HighlightStyle::Custom(2)) => 2,
    Some(HighlightStyle::Custom(3)) => 3,
    Some(HighlightStyle::Custom(slot)) => 128 + (slot & 0x7f),
  }
}

#[hotpath::measure]
fn decode_highlight_style(value: u8) -> io::Result<Option<HighlightStyle>> {
  Ok(match value {
    0 => None,
    1 => Some(HighlightStyle::Custom(1)),
    2 => Some(HighlightStyle::Custom(2)),
    3 => Some(HighlightStyle::Custom(3)),
    128..=255 => Some(HighlightStyle::Custom(value - 128)),
    _ => {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "highlight slot is reserved but has no app style yet",
      ));
    },
  })
}
