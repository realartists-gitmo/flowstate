
fn read_u8(cursor: &mut Cursor<&[u8]>) -> io::Result<u8> {
  let mut bytes = [0; 1];
  cursor.read_exact(&mut bytes)?;
  Ok(bytes[0])
}

fn read_u16(cursor: &mut Cursor<&[u8]>) -> io::Result<u16> {
  let mut bytes = [0; 2];
  cursor.read_exact(&mut bytes)?;
  Ok(u16::from_le_bytes(bytes))
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> io::Result<u32> {
  let mut bytes = [0; 4];
  cursor.read_exact(&mut bytes)?;
  Ok(u32::from_le_bytes(bytes))
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> io::Result<u64> {
  let mut bytes = [0; 8];
  cursor.read_exact(&mut bytes)?;
  Ok(u64::from_le_bytes(bytes))
}

fn read_u128(cursor: &mut Cursor<&[u8]>) -> io::Result<u128> {
  let mut bytes = [0; 16];
  cursor.read_exact(&mut bytes)?;
  Ok(u128::from_le_bytes(bytes))
}

fn write_u64(bytes: &mut Vec<u8>, value: u64) {
  bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_u128(bytes: &mut Vec<u8>, value: u128) {
  bytes.extend_from_slice(&value.to_le_bytes());
}

fn read_len(cursor: &mut Cursor<&[u8]>, label: &'static str) -> io::Result<usize> {
  let raw = read_u64(cursor)?;
  usize::try_from(raw).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{label} overflows usize")))
}

fn read_bytes<'a>(cursor: &mut Cursor<&'a [u8]>, len: usize, label: &'static str) -> io::Result<&'a [u8]> {
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

fn read_string(cursor: &mut Cursor<&[u8]>) -> io::Result<String> {
  let len = read_len(cursor, "DB8 string length")?;
  let bytes = read_bytes(cursor, len, "DB8 string")?;
  std::str::from_utf8(bytes)
    .map(|text| text.to_owned())
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "DB8 string is not UTF-8"))
}

fn write_string(bytes: &mut Vec<u8>, value: &str) {
  write_u64(bytes, value.len() as u64);
  bytes.extend_from_slice(value.as_bytes());
}

fn encode_block_alignment(alignment: BlockAlignment) -> u8 {
  match alignment {
    BlockAlignment::Left => 0,
    BlockAlignment::Center => 1,
    BlockAlignment::Right => 2,
  }
}

fn decode_block_alignment(value: u8) -> io::Result<BlockAlignment> {
  match value {
    0 => Ok(BlockAlignment::Left),
    1 => Ok(BlockAlignment::Center),
    2 => Ok(BlockAlignment::Right),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid block alignment")),
  }
}

fn encode_paragraph_style(style: ParagraphStyle) -> u8 {
  match style {
    ParagraphStyle::Pocket => 0,
    ParagraphStyle::Hat => 1,
    ParagraphStyle::Block => 2,
    ParagraphStyle::Tag => 3,
    ParagraphStyle::Analytic => 4,
    ParagraphStyle::Normal => 5,
    ParagraphStyle::Undertag => 6,
  }
}

fn decode_paragraph_style(value: u8) -> io::Result<ParagraphStyle> {
  match value {
    0 => Ok(ParagraphStyle::Pocket),
    1 => Ok(ParagraphStyle::Hat),
    2 => Ok(ParagraphStyle::Block),
    3 => Ok(ParagraphStyle::Tag),
    4 => Ok(ParagraphStyle::Analytic),
    5 => Ok(ParagraphStyle::Normal),
    6 => Ok(ParagraphStyle::Undertag),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid paragraph style")),
  }
}

fn write_run_styles(bytes: &mut Vec<u8>, styles: RunStyles) {
  bytes.push(encode_run_semantic_style(styles.semantic));
  let mut flags = 0u8;
  if styles.direct_underline {
    flags |= 1 << 0;
  }
  if styles.strikethrough {
    flags |= 1 << 1;
  }
  bytes.push(flags);
  bytes.push(encode_highlight_style(styles.highlight));
}

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

fn encode_run_semantic_style(style: RunSemanticStyle) -> u8 {
  match style {
    RunSemanticStyle::Plain => 0,
    RunSemanticStyle::Cite => 1,
    RunSemanticStyle::Emphasis => 2,
    RunSemanticStyle::Underline => 3,
    RunSemanticStyle::Condensed => 4,
    RunSemanticStyle::Ultracondensed => 5,
  }
}

fn decode_run_semantic_style(value: u8) -> io::Result<RunSemanticStyle> {
  match value {
    0 => Ok(RunSemanticStyle::Plain),
    1 => Ok(RunSemanticStyle::Cite),
    2 => Ok(RunSemanticStyle::Emphasis),
    3 => Ok(RunSemanticStyle::Underline),
    4 => Ok(RunSemanticStyle::Condensed),
    5 => Ok(RunSemanticStyle::Ultracondensed),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid run semantic style")),
  }
}

fn encode_highlight_style(style: Option<HighlightStyle>) -> u8 {
  match style {
    None => 0,
    Some(HighlightStyle::Spoken) => 1,
    Some(HighlightStyle::Insert) => 2,
    Some(HighlightStyle::Alternative) => 3,
  }
}

fn decode_highlight_style(value: u8) -> io::Result<Option<HighlightStyle>> {
  if value > 31 {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid highlight style slot"));
  }
  Ok(match value {
    0 => None,
    1 => Some(HighlightStyle::Spoken),
    2 => Some(HighlightStyle::Insert),
    3 => Some(HighlightStyle::Alternative),
    _ => {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "highlight slot is reserved but has no app style yet",
      ));
    },
  })
}
