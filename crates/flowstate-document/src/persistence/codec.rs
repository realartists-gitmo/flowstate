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
  let len = read_len(cursor, "DB8 string length")?;
  let bytes = read_bytes(cursor, len, "DB8 string")?;
  std::str::from_utf8(bytes)
    .map(std::borrow::ToOwned::to_owned)
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 string is not UTF-8"))
}

#[hotpath::measure]
fn write_string(bytes: &mut Vec<u8>, value: &str) {
  write_u64(bytes, value.len() as u64);
  bytes.extend_from_slice(value.as_bytes());
}

#[hotpath::measure]
fn encode_block_alignment(alignment: BlockAlignment) -> u8 {
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

#[hotpath::measure]
fn encode_paragraph_style(style: ParagraphStyle) -> u8 {
  if style == PARAGRAPH_POCKET {
    0
  } else if style == PARAGRAPH_HAT {
    1
  } else if style == PARAGRAPH_BLOCK {
    2
  } else if style == PARAGRAPH_TAG {
    3
  } else if style == PARAGRAPH_ANALYTIC {
    4
  } else if style == ParagraphStyle::Normal {
    5
  } else if style == PARAGRAPH_UNDERTAG {
    6
  } else {
    5
  }
}

#[hotpath::measure]
fn decode_paragraph_style(value: u8) -> io::Result<ParagraphStyle> {
  match value {
    0 => Ok(PARAGRAPH_POCKET),
    1 => Ok(PARAGRAPH_HAT),
    2 => Ok(PARAGRAPH_BLOCK),
    3 => Ok(PARAGRAPH_TAG),
    4 => Ok(PARAGRAPH_ANALYTIC),
    5 => Ok(ParagraphStyle::Normal),
    6 => Ok(PARAGRAPH_UNDERTAG),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid paragraph style")),
  }
}

#[hotpath::measure]
fn encode_section_kind(kind: SectionKind) -> u8 {
  match kind {
    SectionKind::Custom(value) => value,
  }
}

#[hotpath::measure]
fn decode_section_kind(value: u8) -> io::Result<SectionKind> {
  Ok(SectionKind::Custom(value))
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

#[hotpath::measure]
fn encode_run_semantic_style(style: RunSemanticStyle) -> u8 {
  if style == RunSemanticStyle::Plain {
    0
  } else if style == SEMANTIC_CITE {
    1
  } else if style == SEMANTIC_EMPHASIS {
    2
  } else if style == SEMANTIC_UNDERLINE {
    3
  } else if style == SEMANTIC_CONDENSED {
    4
  } else if style == SEMANTIC_ULTRACONDENSED {
    5
  } else if let RunSemanticStyle::Custom(slot) = style {
    slot.wrapping_add(6)
  } else {
    0
  }
}

#[hotpath::measure]
fn decode_run_semantic_style(value: u8) -> io::Result<RunSemanticStyle> {
  Ok(match value {
    0 => RunSemanticStyle::Plain,
    1 => SEMANTIC_CITE,
    2 => SEMANTIC_EMPHASIS,
    3 => SEMANTIC_UNDERLINE,
    4 => SEMANTIC_CONDENSED,
    5 => SEMANTIC_ULTRACONDENSED,
    slot => RunSemanticStyle::Custom(slot.wrapping_sub(6)),
  })
}

#[hotpath::measure]
fn encode_highlight_style(style: Option<HighlightStyle>) -> u8 {
  match style {
    None => 0,
    Some(HIGHLIGHT_SPOKEN) => 1,
    Some(HIGHLIGHT_INSERT) => 2,
    Some(HIGHLIGHT_ALTERNATIVE) => 3,
    Some(HighlightStyle::Custom(slot)) => slot,
  }
}

#[hotpath::measure]
fn decode_highlight_style(value: u8) -> io::Result<Option<HighlightStyle>> {
  Ok(match value {
    0 => None,
    1 => Some(HIGHLIGHT_SPOKEN),
    2 => Some(HIGHLIGHT_INSERT),
    3 => Some(HIGHLIGHT_ALTERNATIVE),
    slot => Some(HighlightStyle::Custom(slot)),
  })
}
