//! Native TI Connect Evo file formats. No TI library is linked or distributed.
//! Evidence: public Connect Evo 7.1.0.792 client and controlled conversion probes.

use ciborium::Value;
use serde::{Deserialize, Serialize};
use std::io::Cursor;

const MAX_FILE: usize = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct FileError(pub String);
pub type Result<T> = std::result::Result<T, FileError>;
fn err(s: impl Into<String>) -> FileError {
    FileError(s.into())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    /// Hexadecimal 16-bit tokens, in display order (e.g. E830 for L1).
    pub token_name: String,
    pub kind: u32,
    pub size: usize,
    pub archived: bool,
    pub extension: String,
}

#[derive(Clone, Debug)]
pub struct VariableFile {
    pub variable: Variable,
    /// Native CBOR payload. Locally exported files may additionally carry a checksum.
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub enum CsvMode {
    /// Empty names select L1..L6, one list per input column.
    Lists { names: Vec<String> },
    /// Zero rows/columns infer the input dimensions. Explicit dimensions crop/pad zeros.
    Matrix {
        name: String,
        rows: usize,
        columns: usize,
    },
}

pub fn extension(kind: u32) -> String {
    match kind {
        0 | 16 => ".8xn2",
        1 => ".8xl2",
        2 => ".8xp2",
        3 => ".8xd2",
        4 => ".8ci2",
        5 => ".8ca2",
        6 => ".8xm2",
        7 => ".8xy2",
        8 => ".8xv2",
        9 => ".8xg2",
        10 => ".8xs2",
        11 => ".8ek2",
        12 => ".8xw2",
        13 => ".8xz2",
        14 => ".8xt2",
        15 => ".py",
        18 => ".8mp2",
        _ => return format!(".at{kind}"),
    }
    .into()
}

pub fn type_description(kind: u32) -> &'static str {
    match kind {
        0 | 16 => "Number",
        1 => "List",
        2 => "Program",
        3 => "Graph database",
        4 => "Picture",
        5 => "Background image",
        6 => "Matrix",
        7 => "Equation",
        8 => "App variable",
        9 => "Group",
        10 => "String",
        11 => "App",
        12 => "Window",
        13 => "User zoom",
        14 => "Table setup",
        15 => "Python program",
        18 => "Python module",
        _ => "Unknown",
    }
}

fn field<'a>(value: &'a Value, name: &str) -> Result<Option<&'a Value>> {
    let Value::Map(entries) = value else {
        return Err(err("Expected a CBOR map"));
    };
    let mut matches = entries.iter().filter(|(k, _)| k.as_text() == Some(name));
    let result = matches.next().map(|(_, v)| v);
    if matches.next().is_some() {
        return Err(err(format!("Duplicate {name} field")));
    }
    Ok(result)
}
fn required<'a>(value: &'a Value, name: &str) -> Result<&'a Value> {
    field(value, name)?.ok_or_else(|| err(format!("Missing {name} field")))
}
fn number(value: &Value) -> Result<u64> {
    match value {
        Value::Integer(n) => u64::try_from(*n).map_err(|_| err("Negative or excessive integer")),
        Value::Text(s) => s.parse().map_err(|_| err("Invalid numeric field")),
        _ => Err(err("Expected an integer")),
    }
}

// The official WEB_addCRC routine calls this a CRC, but uses an XOR of LE words
// excluding its final 3/4 bytes, then appends the result big-endian. Preserve that
// observed file convention; the USB/Kermit transport has its own independent CRC.
fn file_checksum(bytes: &[u8]) -> u16 {
    let n = bytes.len().saturating_sub(3);
    let words = (n / 2).saturating_sub(n & 1);
    bytes[..words * 2]
        .as_chunks::<2>()
        .0
        .iter()
        .fold(0, |sum, word| sum ^ u16::from_le_bytes(*word))
}

fn parse(bytes: &[u8]) -> Result<(Value, usize)> {
    if bytes.is_empty() || bytes.len() > MAX_FILE {
        return Err(err("File is empty or exceeds 16 MiB"));
    }
    let mut cursor = Cursor::new(bytes);
    let value =
        ciborium::from_reader(&mut cursor).map_err(|e| err(format!("Invalid CBOR: {e}")))?;
    let used = cursor.position() as usize;
    let trailing = &bytes[used..];
    if !trailing.is_empty()
        && (trailing.len() != 2 || trailing != file_checksum(&bytes[..used]).to_be_bytes())
    {
        return Err(err("Invalid file checksum or trailing data"));
    }
    Ok((value, used))
}

fn encode(value: &Value) -> Result<Vec<u8>> {
    // Indefinite maps match the official client and the guest's CBOR producers.
    fn write(value: &Value, out: &mut Vec<u8>) -> Result<()> {
        if let Value::Map(fields) = value {
            out.push(0xbf);
            for (key, value) in fields {
                write(key, out)?;
                write(value, out)?;
            }
            out.push(0xff);
            Ok(())
        } else {
            ciborium::into_writer(value, out).map_err(|e| err(format!("Cannot encode CBOR: {e}")))
        }
    }
    let mut bytes = Vec::new();
    write(value, &mut bytes)?;
    Ok(bytes)
}
fn map(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Map(
        entries
            .into_iter()
            .map(|(k, v)| (Value::Text(k.into()), v))
            .collect(),
    )
}
fn integer(n: u64) -> Value {
    Value::Integer(n.into())
}
fn token_bytes(tokens: &str) -> Result<Vec<u8>> {
    if tokens.is_empty()
        || tokens.len() > 36
        || !tokens.len().is_multiple_of(4)
        || !tokens.is_ascii()
    {
        return Err(err("Invalid token name length"));
    }
    let mut bytes = Vec::new();
    for chunk in tokens.as_bytes().as_chunks::<4>().0 {
        let token = u16::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16)
            .map_err(|_| err("Invalid token name"))?;
        bytes.extend_from_slice(&token.to_le_bytes());
    }
    Ok(bytes)
}
fn tokens_from_bytes(bytes: &[u8]) -> Result<String> {
    if bytes.is_empty() || bytes.len() > 36 || !bytes.len().is_multiple_of(2) {
        return Err(err("Invalid token-name bytes"));
    }
    let mut tokens: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|v| u16::from_le_bytes(*v))
        .collect();
    while tokens.last() == Some(&0) {
        tokens.pop();
    }
    if tokens.is_empty() {
        return Err(err("Empty token name"));
    }
    Ok(tokens.iter().map(|v| format!("{v:04X}")).collect())
}

pub fn display_name(token_name: &str, kind: u32) -> Result<String> {
    let raw = token_bytes(token_name)?;
    let words: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|v| u16::from_le_bytes(*v))
        .filter(|v| *v != 0)
        .collect();
    if words.len() == 1 {
        let t = words[0];
        let indexed = |base: u16, prefix: &str| format!("{prefix}{}", (t - base + 1) % 10);
        let fixed = match (kind, t) {
            (1, 0xe830..=0xe835) => Some(format!("L{}", t - 0xe830 + 1)),
            (6, 0xe820..=0xe829) => Some(char::from(b'A' + (t - 0xe820) as u8).to_string()),
            (3, 0xe890..=0xe899) => Some(indexed(0xe890, "GDB")),
            (4, 0xe880..=0xe889) => Some(indexed(0xe880, "Pic")),
            (5, 0xe8b0..=0xe8b9) => Some(indexed(0xe8b0, "Image")),
            (10, 0xe8a0..=0xe8a9) => Some(indexed(0xe8a0, "Str")),
            (7, 0xe840..=0xe849) => Some(indexed(0xe840, "Y")),
            (7, 0xe850..=0xe85b) => Some(format!(
                "{}{}T",
                if (t - 0xe850).is_multiple_of(2) {
                    'X'
                } else {
                    'Y'
                },
                (t - 0xe850) / 2 + 1
            )),
            (7, 0xe860..=0xe865) => Some(format!("r{}", t - 0xe860 + 1)),
            (7, 0xe870..=0xe872) => Some(char::from(b'u' + (t - 0xe870) as u8).to_string()),
            (12, 0xe8ba) => Some("Window".into()),
            (13, 0xe8bb) => Some("RclWindw".into()),
            (14, 0xe8bc) => Some("TblSet".into()),
            _ => None,
        };
        if let Some(name) = fixed {
            return Ok(name);
        }
    }
    let mut name = String::new();
    for (i, t) in words.into_iter().enumerate() {
        if i == 0 && kind == 1 && t == 0xe836 {
            continue;
        }
        name.push(match t {
            0xe800..=0xe819 => char::from(b'A' + (t - 0xe800) as u8),
            0xe401..=0xe40a => char::from(b'0' + (t - 0xe401) as u8),
            0xe81a => 'θ',
            0xe400 | 0x5f => '_',
            0x61..=0x7a => char::from(t as u8),
            _ => return Ok(format!("[{token_name}]")),
        });
    }
    Ok(name)
}

pub fn encode_name(name: &str, kind: u32) -> Result<String> {
    let name = name.trim().to_uppercase();
    // Fixed-name types must stay in their own name table.
    for token in 0xe800u16..=0xe8bc {
        let hex = format!("{token:04X}");
        if display_name(&hex, kind)?.to_uppercase() == name && fixed_name(kind, token) {
            return Ok(hex);
        }
    }
    if !matches!(kind, 1 | 2 | 8 | 9 | 11 | 15 | 18) {
        return Err(err("This variable requires a predefined calculator name"));
    }
    let limit = if kind == 1 { 5 } else { 8 };
    if name.is_empty() || name.chars().count() > limit {
        return Err(err(format!("Name must contain 1–{limit} characters")));
    }
    let first = name.chars().next().unwrap();
    if !first.is_ascii_uppercase() && !(first == 'Θ' && !matches!(kind, 15 | 18)) {
        return Err(err("Name must begin with a letter"));
    }
    let mut out = if kind == 1 {
        "E836".to_string()
    } else {
        String::new()
    };
    for c in name.chars() {
        let t = match c {
            'A'..='Z' => 0xe800 + c as u16 - 'A' as u16,
            '0'..='9' => 0xe401 + c as u16 - '0' as u16,
            'Θ' if !matches!(kind, 15 | 18) => 0xe81a,
            '_' if matches!(kind, 15 | 18) => 0xe400,
            _ => return Err(err("Name contains an unsupported character")),
        };
        out.push_str(&format!("{t:04X}"));
    }
    Ok(out)
}
fn fixed_name(kind: u32, t: u16) -> bool {
    match kind {
        0 | 16 => (0xe800..=0xe81a).contains(&t),
        1 => (0xe830..=0xe835).contains(&t),
        3 => (0xe890..=0xe899).contains(&t),
        4 => (0xe880..=0xe889).contains(&t),
        5 => (0xe8b0..=0xe8b9).contains(&t),
        6 => (0xe820..=0xe829).contains(&t),
        7 => {
            (0xe840..=0xe849).contains(&t)
                || (0xe850..=0xe85b).contains(&t)
                || (0xe860..=0xe865).contains(&t)
                || (0xe870..=0xe872).contains(&t)
        }
        10 => (0xe8a0..=0xe8a9).contains(&t),
        12 => t == 0xe8ba,
        13 => t == 0xe8bb,
        14 => t == 0xe8bc,
        _ => false,
    }
}

fn variable(metadata: &Value, size: usize) -> Result<Variable> {
    let kind = u32::try_from(number(required(metadata, "type")?)?)
        .map_err(|_| err("Invalid variable type"))?;
    let token_name = match required(metadata, "name")? {
        Value::Bytes(b) => tokens_from_bytes(b)?,
        Value::Text(s) => {
            token_bytes(s)?;
            s.trim_end_matches("0000").to_uppercase()
        }
        _ => return Err(err("Invalid variable name field")),
    };
    let archived = field(metadata, "flags")?
        .map(number)
        .transpose()?
        .unwrap_or(0)
        & 1
        != 0;
    Ok(Variable {
        name: display_name(&token_name, kind)?,
        token_name,
        kind,
        size,
        archived,
        extension: extension(kind),
    })
}

pub fn decode_variable(bytes: &[u8]) -> Result<VariableFile> {
    let (value, used) = parse(bytes)?;
    let metadata = required(&value, "metaData")?;
    number(required(&value, "version")?)?;
    // Numeric/list/matrix variables have typed CBOR fields, not a byte-data envelope.
    let size = field(&value, "size")?
        .map(number)
        .transpose()?
        .map_or(Ok(used), usize::try_from)
        .map_err(|_| err("Invalid variable size"))?;
    if let Some(data) = field(&value, "data")?.and_then(Value::as_bytes)
        && field(&value, "size")?.is_some()
        && size != data.len()
    {
        return Err(err("Variable size does not match its payload"));
    }
    Ok(VariableFile {
        variable: variable(metadata, size)?,
        bytes: bytes[..used].to_vec(),
    })
}

pub fn prepare_native(
    bytes: &[u8],
    name_override: Option<&str>,
    archive_override: Option<bool>,
) -> Result<VariableFile> {
    let file = decode_variable(bytes)?;
    if file.variable.kind == 15
        && let Some(name) = name_override
    {
        return encode_python(
            name,
            &python_source(bytes)?,
            archive_override.unwrap_or(file.variable.archived),
        );
    }
    let (mut value, _) = parse(bytes)?;
    let Value::Map(root) = &mut value else {
        unreachable!()
    };
    let (_, Value::Map(metadata)) = root
        .iter_mut()
        .find(|(k, _)| k.as_text() == Some("metaData"))
        .unwrap()
    else {
        return Err(err("Invalid metadata"));
    };
    let mut set = |name: &str, value: Value| {
        if let Some((_, v)) = metadata.iter_mut().find(|(k, _)| k.as_text() == Some(name)) {
            *v = value
        } else {
            metadata.push((Value::Text(name.into()), value));
        }
    };
    if let Some(name) = name_override {
        set(
            "name",
            Value::Bytes(token_bytes(&encode_name(name, file.variable.kind)?)?),
        );
    }
    if let Some(archived) = archive_override {
        validate_memory(file.variable.kind, archived)?;
        let flags = field(required(&parse(bytes)?.0, "metaData")?, "flags")?
            .map(number)
            .transpose()?
            .unwrap_or(0);
        set("flags", integer((flags & !1) | u64::from(archived)));
    }
    decode_variable(&encode(&value)?)
}

fn validate_memory(kind: u32, archived: bool) -> Result<()> {
    if (matches!(kind, 4 | 5 | 9 | 11 | 18) && !archived)
        || (matches!(kind, 7 | 12 | 13 | 14) && archived)
    {
        return Err(err(
            "This variable type does not support the chosen memory location",
        ));
    }
    Ok(())
}

fn native(metadata: Value, data: Vec<u8>) -> Result<VariableFile> {
    decode_variable(&encode(&map([
        ("metaData", metadata),
        ("version", integer(1)),
        ("size", integer(data.len() as u64)),
        ("data", Value::Bytes(data)),
    ]))?)
}

pub fn encode_python(name: &str, source: &[u8], archived: bool) -> Result<VariableFile> {
    std::str::from_utf8(source).map_err(|_| err("Python source must be UTF-8"))?;
    if source.is_empty() || source.contains(&0) || source.len() > MAX_FILE - 1024 {
        return Err(err("Python source is empty, contains NUL, or is too large"));
    }
    let name = name
        .strip_suffix(".py")
        .or_else(|| name.strip_suffix(".PY"))
        .unwrap_or(name)
        .to_uppercase();
    let tokens = encode_name(&name, 15)?;
    let total = 12 + name.len() + 1 + 4 + source.len() + 1;
    let mut data = Vec::with_capacity(total);
    data.extend_from_slice(&0x113u32.to_le_bytes());
    data.extend_from_slice(&(total as u32).to_le_bytes());
    data.extend_from_slice(&(name.len() as u32).to_le_bytes());
    data.extend_from_slice(name.as_bytes());
    data.push(0);
    data.extend_from_slice(&(source.len() as u32 | 0x0200_0000).to_le_bytes());
    data.extend_from_slice(source);
    data.push(0);
    let mut entries = vec![
        ("type", integer(15)),
        ("version", integer(1)),
        ("name", Value::Bytes(token_bytes(&tokens)?)),
    ];
    if archived {
        entries.push(("flags", integer(1)));
    }
    native(map(entries), data)
}

fn python_source(bytes: &[u8]) -> Result<Vec<u8>> {
    let (value, _) = parse(bytes)?;
    let data = required(&value, "data")?
        .as_bytes()
        .ok_or_else(|| err("Missing Python data"))?;
    let word = |offset: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(
            data.get(offset..offset + 4)
                .ok_or_else(|| err("Truncated Python header"))?
                .try_into()
                .unwrap(),
        ))
    };
    if word(0)? != 0x113 || word(4)? as usize != data.len() {
        return Err(err("Invalid Python file header"));
    }
    let namesize = word(8)? as usize;
    if !(1..=8).contains(&namesize) || data.get(12 + namesize) != Some(&0) {
        return Err(err("Invalid Python name"));
    }
    let packed = word(13 + namesize)?;
    let len = (packed & 0xffffff) as usize;
    let begin = 17 + namesize;
    if packed >> 24 != 2
        || begin.checked_add(len + 1) != Some(data.len())
        || data.last() != Some(&0)
    {
        return Err(err("Invalid Python source bounds"));
    }
    let source = &data[begin..begin + len];
    std::str::from_utf8(source).map_err(|_| err("Python source is not UTF-8"))?;
    Ok(source.to_vec())
}

pub fn encode_image(slot: u8, rgba: &[u8]) -> Result<VariableFile> {
    if !(1..=10).contains(&slot) || rgba.len() != 160 * 105 * 4 {
        return Err(err("Image requires slot 1–10 and 160×105 RGBA pixels"));
    }
    let mut data = Vec::with_capacity(33601);
    data.push(11);
    for y in (0..105).rev() {
        for x in 0..160 {
            let p = &rgba[(y * 160 + x) * 4..][..4];
            // The browser composites transparent source pixels over white before conversion.
            let channel = |c: u8| {
                (u32::from(c) * u32::from(p[3]) + 255 * (255 - u32::from(p[3])) + 127) / 255
            };
            let r = (channel(p[0]) * 31 + 127) / 255;
            let g = (channel(p[1]) * 63 + 127) / 255;
            let b = (channel(p[2]) * 31 + 127) / 255;
            data.extend_from_slice(&((r << 11 | g << 5 | b) as u16).to_le_bytes());
        }
    }
    let mut name = (0xe8b0u16 + u16::from(slot) - 1).to_le_bytes().to_vec();
    name.extend_from_slice(&[0, 0]);
    native(
        map([
            ("type", integer(5)),
            ("version", integer(1)),
            ("flags", integer(1)),
            ("name", Value::Bytes(name)),
        ]),
        data,
    )
}

pub fn decode_screen(bytes: &[u8]) -> Result<Vec<u8>> {
    let envelope;
    let bytes = if bytes.len() == 320 * 240 * 2 {
        bytes
    } else {
        envelope = parse(bytes)?.0;
        if number(required(&envelope, "width")?)? != 320
            || number(required(&envelope, "height")?)? != 240
            || number(required(&envelope, "bpp")?)? != 16
        {
            return Err(err("Unsupported screenshot geometry"));
        }
        required(&envelope, "data")?
            .as_bytes()
            .ok_or_else(|| err("Screenshot has no pixel bytes"))?
    };
    if bytes.len() != 320 * 240 * 2 {
        return Err(err("Screenshot must contain 320×240 RGB565 pixels"));
    }
    let mut out = Vec::with_capacity(320 * 240 * 4);
    for p in bytes.as_chunks::<2>().0 {
        let v = u16::from_le_bytes(*p);
        out.extend_from_slice(&[
            ((u32::from(v >> 11) * 255 + 15) / 31) as u8,
            ((u32::from((v >> 5) & 63) * 255 + 31) / 63) as u8,
            ((u32::from(v & 31) * 255 + 15) / 31) as u8,
            255,
        ]);
    }
    Ok(out)
}

pub fn export_variable(bytes: &[u8]) -> Result<(String, Vec<u8>)> {
    let file = decode_variable(bytes)?;
    let output = if file.variable.kind == 15 {
        python_source(bytes)?
    } else {
        let mut output = file.bytes;
        let crc = file_checksum(&output);
        output.extend_from_slice(&crc.to_be_bytes());
        output
    };
    let name = file
        .variable
        .name
        .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
    Ok((format!("{name}{}", file.variable.extension), output))
}

pub fn parse_info(bytes: &[u8]) -> Result<serde_json::Value> {
    fn json(v: Value) -> Result<serde_json::Value> {
        Ok(match v {
            Value::Text(s) => s.into(),
            Value::Bool(b) => b.into(),
            Value::Null => serde_json::Value::Null,
            Value::Integer(n) => {
                let n = i128::from(n);
                if let Ok(n) = i64::try_from(n) {
                    n.into()
                } else if let Ok(n) = u64::try_from(n) {
                    n.into()
                } else {
                    return Err(err("JSON integer overflow"));
                }
            }
            Value::Float(f) => serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .ok_or_else(|| err("Nonfinite information value"))?,
            Value::Bytes(b) => serde_json::Value::Array(b.into_iter().map(Into::into).collect()),
            Value::Array(a) => {
                serde_json::Value::Array(a.into_iter().map(json).collect::<Result<_>>()?)
            }
            Value::Map(m) => {
                let mut out = serde_json::Map::new();
                for (k, v) in m {
                    let Value::Text(k) = k else {
                        return Err(err("Nontext information key"));
                    };
                    if out.insert(k, json(v)?).is_some() {
                        return Err(err("Duplicate information key"));
                    }
                }
                serde_json::Value::Object(out)
            }
            _ => return Err(err("Unsupported information value")),
        })
    }
    json(parse(bytes)?.0)
}

pub fn parse_directory(bytes: &[u8]) -> Result<Vec<Variable>> {
    let (value, _) = parse(bytes)?;
    let metadata = required(&value, "metaData")?;
    if number(required(metadata, "type")?)? != 56 {
        return Err(err("Response is not a directory"));
    }
    let Value::Array(entries) = required(&value, "data")? else {
        return Err(err("Directory entries must be a CBOR array"));
    };
    entries
        .iter()
        .map(|entry| {
            let size = usize::try_from(number(required(entry, "size")?)?)
                .map_err(|_| err("Invalid directory size"))?;
            let kind = u32::try_from(number(required(entry, "type")?)?)
                .map_err(|_| err("Invalid directory type"))?;
            let token_name = tokens_from_bytes(
                required(entry, "tokName")?
                    .as_bytes()
                    .ok_or_else(|| err("Invalid directory token name"))?,
            )?;
            let archived = required(entry, "mem")?
                .as_bool()
                .ok_or_else(|| err("Invalid directory memory flag"))?;
            Ok(Variable {
                name: display_name(&token_name, kind)?,
                token_name,
                kind,
                size,
                archived,
                extension: extension(kind),
            })
        })
        .collect()
}

pub fn encode_csv(csv: &str, mode: CsvMode, archived: bool) -> Result<Vec<VariableFile>> {
    if csv.len() > MAX_FILE {
        return Err(err("CSV exceeds 16 MiB"));
    }
    let normalized = csv.replace("\r\n", "\n").replace('\r', "\n");
    let table: Vec<Vec<&str>> = normalized
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split(',').map(str::trim).collect())
        .collect();
    if table.is_empty() || table.len() > 999 {
        return Err(err("CSV must contain 1–999 rows"));
    }
    let numeric = |text: &str| -> Result<String> {
        if text.is_empty() {
            return Ok("0".into());
        }
        if !text
            .bytes()
            .all(|c| c.is_ascii_digit() || b"+-.eE".contains(&c))
            || !text.parse::<f64>().is_ok_and(f64::is_finite)
        {
            return Err(err(format!("Invalid CSV number: {text}")));
        }
        Ok(text.replace(['e', 'E'], "@E"))
    };
    let ascii = |name: &str, kind: u32, expression: String| -> Result<VariableFile> {
        let tokens = encode_name(name, kind)?;
        let display = display_name(&tokens, kind)?;
        let wire_name = if kind == 1 && tokens.len() == 4 {
            display[1..].to_string()
        } else {
            display.replace('θ', "theta")
        };
        let bytes = encode(&map([
            (
                "metaData",
                map([("type", integer(60)), ("version", integer(1))]),
            ),
            (
                "type",
                Value::Text(if kind == 1 { "List" } else { "Matrix" }.into()),
            ),
            ("version", integer(1)),
            ("name", Value::Text(wire_name)),
            ("data", Value::Bytes(expression.as_bytes().to_vec())),
        ]))?;
        Ok(VariableFile {
            variable: Variable {
                name: display,
                token_name: tokens,
                kind,
                size: expression.len(),
                archived,
                extension: extension(kind),
            },
            bytes,
        })
    };
    match mode {
        CsvMode::Lists { mut names } => {
            let count = table.iter().map(Vec::len).max().unwrap();
            if names.is_empty() {
                if count > 6 {
                    return Err(err("Lists accept at most six columns"));
                }
                names = (1..=count).map(|n| format!("L{n}")).collect();
            }
            if names.is_empty() || names.len() > 6 || names.len() > count {
                return Err(err("Select one to six existing CSV columns"));
            }
            let mut files = Vec::new();
            for (column, name) in names.iter().enumerate() {
                let mut values = Vec::new();
                let mut gap = false;
                for row in &table {
                    let v = row.get(column).copied().unwrap_or("");
                    if v.is_empty() {
                        gap = true
                    } else {
                        if gap {
                            return Err(err(
                                "List columns may only have empty cells after their final value",
                            ));
                        }
                        values.push(numeric(v)?);
                    }
                }
                if values.is_empty() {
                    return Err(err("List column is empty"));
                }
                files.push(ascii(name, 1, format!("{{{}}}", values.join(",")))?);
            }
            Ok(files)
        }
        CsvMode::Matrix {
            name,
            rows,
            columns,
        } => {
            let rows = if rows == 0 { table.len() } else { rows };
            let columns = if columns == 0 {
                table.iter().map(Vec::len).max().unwrap()
            } else {
                columns
            };
            if rows == 0 || columns == 0 || rows > 99 || columns > 99 || rows * columns > 400 {
                return Err(err(
                    "Matrix requires 1–99 rows/columns and at most 400 cells",
                ));
            }
            let mut expression = String::from("[");
            for row in 0..rows {
                let values = (0..columns)
                    .map(|col| {
                        numeric(
                            table
                                .get(row)
                                .and_then(|r| r.get(col))
                                .copied()
                                .unwrap_or("0"),
                        )
                    })
                    .collect::<Result<Vec<_>>>()?;
                expression.push('[');
                expression.push_str(&values.join(","));
                expression.push(']');
            }
            expression.push(']');
            Ok(vec![ascii(&name, 6, expression)?])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn hex(s: &str) -> Vec<u8> {
        s.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|v| u8::from_str_radix(std::str::from_utf8(v).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn python_matches_official_conversion_and_roundtrips_source() {
        // Captured from the unmodified public TI client converting this synthetic source.
        let expected = hex(
            "bf686d65746144617461bf64747970650f6776657273696f6e01646e616d654a07e804e80be80be80ee8ff6776657273696f6e016473697a6518216464617461582113010000210000000500000048454c4c4f000a0000027072696e74283432290a00ffdfaa",
        );
        let file = encode_python("HELLO.py", b"print(42)\n", false).unwrap();
        assert_eq!(file.bytes, &expected[..expected.len() - 2]);
        assert_eq!(
            file_checksum(&file.bytes).to_be_bytes(),
            expected[expected.len() - 2..]
        );
        assert_eq!(decode_variable(&expected).unwrap().variable, file.variable);
        assert_eq!(
            export_variable(&expected).unwrap(),
            ("HELLO.py".into(), b"print(42)\n".to_vec())
        );
        let renamed = prepare_native(&expected, Some("NEW_NAME"), Some(true)).unwrap();
        assert_eq!(renamed.variable.name, "NEW_NAME");
        assert!(renamed.variable.archived);
        assert_eq!(export_variable(&renamed.bytes).unwrap().1, b"print(42)\n");
        assert!(encode_python("bad-name", b"x=1", false).is_err());
        assert!(encode_python("ABC", b"x=1\0", false).is_err());
    }

    #[test]
    fn image_matches_official_conversion_orientation_and_quantization() {
        let rgba: Vec<u8> = (0..160 * 105 * 4)
            .map(|i| if i % 4 == 3 { 255 } else { (i * 7 % 256) as u8 })
            .collect();
        let file = encode_image(1, &rgba).unwrap();
        // SHA-256 of TIHelpers.convertImageDataToImageVarData for identical opaque pixels.
        let expected = hex("499cfdbb5e65911f9fe5383b9b8f865aa6634d4c18ffa99c036e6c1a97e450f0");
        assert_eq!(&Sha256::digest(&file.bytes)[..], expected);
        assert_eq!(file.variable.name, "Image1");
        assert!(file.variable.archived);
        assert_eq!(encode_image(10, &rgba).unwrap().variable.name, "Image0");
        let transparent = encode_image(2, &vec![0; 160 * 105 * 4]).unwrap();
        let value = parse(&transparent.bytes).unwrap().0;
        let data = required(&value, "data").unwrap().as_bytes().unwrap();
        assert_eq!(data[0], 11);
        assert!(data[1..].iter().all(|v| *v == 255));
        assert!(encode_image(0, &rgba).is_err());
        assert!(encode_image(1, &rgba[..8]).is_err());
    }

    #[test]
    fn csv_ascii_matches_official_payload_and_validates_dimensions() {
        let file = encode_csv(
            "1\n2e3",
            CsvMode::Lists {
                names: vec!["L1".into()],
            },
            false,
        )
        .unwrap()
        .remove(0);
        let expected = hex(
            "bf686d65746144617461bf6474797065183c6776657273696f6e01ff6474797065644c6973746776657273696f6e01646e616d6561316464617461487b312c324045337dff",
        );
        assert_eq!(file.bytes, expected);
        let list = encode_csv("1,2\r\n3,\r\n", CsvMode::Lists { names: vec![] }, true).unwrap();
        assert_eq!(list.len(), 2);
        let matrix = encode_csv(
            "1,2\n3,4",
            CsvMode::Matrix {
                name: "B".into(),
                rows: 3,
                columns: 2,
            },
            false,
        )
        .unwrap()
        .remove(0);
        let value = parse(&matrix.bytes).unwrap().0;
        assert_eq!(
            required(&value, "data").unwrap().as_bytes().unwrap(),
            b"[[1,2][3,4][0,0]]"
        );
        for bad in ["NaN", "1+2", "Infinity", "1,,3\n2,3,4", "1e999"] {
            assert!(
                encode_csv(bad, CsvMode::Lists { names: vec![] }, false).is_err(),
                "{bad}"
            );
        }
        assert!(
            encode_csv(
                "1",
                CsvMode::Matrix {
                    name: "A".into(),
                    rows: 21,
                    columns: 20
                },
                false
            )
            .is_err()
        );
    }

    #[test]
    fn native_list_preserves_typed_payload_and_unknown_metadata() {
        // Empty-list resource schema observed on the real guest, distinct from byte data.
        let raw = hex(
            "bf686d65746144617461bf6474797065016776657273696f6e0165666c61677300646e616d654430e80000ff6776657273696f6e02647479706500636c656e00ff",
        );
        let file = decode_variable(&raw).unwrap();
        assert_eq!(file.variable.name, "L1");
        let (name, disk) = export_variable(&raw).unwrap();
        assert_eq!(name, "L1.8xl2");
        assert_eq!(decode_variable(&disk).unwrap().bytes, raw);
        let changed = prepare_native(&disk, Some("DATA"), Some(true)).unwrap();
        assert_eq!(changed.variable.token_name, "E836E803E800E813E800");
        assert!(changed.variable.archived);
        assert_eq!(
            number(required(&parse(&changed.bytes).unwrap().0, "len").unwrap()).unwrap(),
            0
        );
        let mut corrupt = disk.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(decode_variable(&corrupt).is_err());
        assert!(decode_variable(&disk[..disk.len() - 1]).is_err());
        assert!(decode_variable(&disk[..10]).is_err());
        assert!(prepare_native(&raw, Some("123BAD"), None).is_err());
    }

    #[test]
    fn directory_and_screenshot_resources_decode_without_ui_conversions() {
        let directory = map([
            (
                "metaData",
                map([("type", integer(56)), ("version", integer(1))]),
            ),
            (
                "data",
                Value::Array(vec![map([
                    ("tokName", Value::Bytes(vec![0x30, 0xe8])),
                    ("dispName", Value::Bytes(vec![b'L', 0xef, 0x81, 0x83])),
                    ("type", integer(1)),
                    ("version", integer(2)),
                    ("size", integer(4)),
                    ("mem", Value::Bool(true)),
                ])]),
            ),
        ]);
        let dir = parse_directory(&encode(&directory).unwrap()).unwrap();
        assert_eq!(
            dir[0],
            Variable {
                name: "L1".into(),
                token_name: "E830".into(),
                kind: 1,
                size: 4,
                archived: true,
                extension: ".8xl2".into()
            }
        );
        let mut pixels = vec![0; 320 * 240 * 2];
        pixels[..6].copy_from_slice(&[0, 0xf8, 0xe0, 7, 0x1f, 0]);
        let screen = map([
            ("width", integer(320)),
            ("height", integer(240)),
            ("bpp", integer(16)),
            ("data", Value::Bytes(pixels.clone())),
        ]);
        let rgba = decode_screen(&encode(&screen).unwrap()).unwrap();
        assert_eq!(
            &rgba[..12],
            &[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255]
        );
        assert_eq!(rgba, decode_screen(&pixels).unwrap());
        assert!(decode_screen(&pixels[..20]).is_err());
    }

    #[test]
    fn external_guest_captures_when_supplied() {
        let Some(path) = std::env::var_os("EVO_CONNECT_FIXTURES") else {
            return;
        };
        let read = |name: &str| std::fs::read(std::path::Path::new(&path).join(name)).unwrap();
        let directory = parse_directory(&read("native-directory.bin")).unwrap();
        assert!(directory.iter().any(|v| v.name == "L1"));
        assert_eq!(
            decode_screen(&read("native-screen.bin")).unwrap().len(),
            320 * 240 * 4
        );
        assert_eq!(
            decode_variable(&read("native-list.bin"))
                .unwrap()
                .variable
                .kind,
            1
        );
        assert!(
            parse_info(&read("native-attrs.bin"))
                .unwrap()
                .get("pkg-version")
                .is_some()
        );
    }
}
