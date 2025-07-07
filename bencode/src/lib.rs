use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Eq, PartialEq)]
pub enum Bencode {
    Int(i64),
    /// We use bytes because not all is utf-8
    Bytes(Vec<u8>),
    List(Vec<Bencode>),
    ///  The keys are sorted in lexicographical order and must be strings
    Dict(BTreeMap<Vec<u8>, Bencode>),
}

#[derive(PartialEq, Eq, Debug, Error)]
pub enum BencodeError {
    #[error("Invalid Bencode format")]
    InvalidBencode,
    #[error("Invalid Bencode number")]
    InvalidBencodeNumber,
    #[error("Invalid Bencode string")]
    InvalidBencodeString,
    #[error("Invalid Bencode list")]
    InvalidBencodeList,
    #[error("Invalid Bencode dictionary")]
    InvalidBencodeDict,
}

pub trait Encode {
    fn to_bencode(&self) -> Bencode;
}

impl Encode for String {
    fn to_bencode(&self) -> Bencode {
        Bencode::Bytes(self.as_bytes().to_vec())
    }
}

impl Bencode {
    pub fn decode(data: &[u8]) -> Result<Bencode, BencodeError> {
        let (bencode, _rest) = Bencode::decode_recurisvely(data)?;
        Ok(bencode)
    }

    fn decode_recurisvely(data: &[u8]) -> Result<(Bencode, &[u8]), BencodeError> {
        if data.is_empty() {
            return Err(BencodeError::InvalidBencode);
        }
        match data[0] {
            b'i' => Bencode::decode_int(data),
            b'0'..=b'9' => Bencode::decode_string(data),
            b'l' => Bencode::decode_list(data),
            b'd' => Bencode::decode_dictionary(data),
            _ => Err(BencodeError::InvalidBencode),
        }
    }

    fn decode_string(data: &[u8]) -> Result<(Bencode, &[u8]), BencodeError> {
        let colon_pos = data
            .iter()
            .position(|&b| b == b':')
            .ok_or(BencodeError::InvalidBencodeString)?;

        let len_part = &data[..colon_pos];
        let rest_after_colon = &data[colon_pos + 1..];

        let len = std::str::from_utf8(len_part)
            .map_err(|_| BencodeError::InvalidBencodeString)?
            .parse::<usize>()
            .map_err(|_| BencodeError::InvalidBencodeString)?;

        if rest_after_colon.len() < len {
            return Err(BencodeError::InvalidBencodeString);
        }

        let string_bytes = &rest_after_colon[..len];
        let rest = &rest_after_colon[len..];

        Ok((Bencode::Bytes(string_bytes.to_vec()), rest))
    }

    fn decode_int(data: &[u8]) -> Result<(Bencode, &[u8]), BencodeError> {
        let end_pos = data[1..]
            .iter()
            .position(|&b| b == b'e')
            .ok_or(BencodeError::InvalidBencodeNumber)?;

        let num_slice = &data[1..=end_pos];
        let num_str = std::str::from_utf8(num_slice).map_err(|_| BencodeError::InvalidBencode)?;
        let num = num_str
            .parse::<i64>()
            .map_err(|_| BencodeError::InvalidBencodeNumber)?;

        let rest = &data[end_pos + 2..];

        Ok((Bencode::Int(num), rest))
    }

    fn decode_list(data: &[u8]) -> Result<(Bencode, &[u8]), BencodeError> {
        let mut elements = Vec::new();
        let mut current_data = &data[1..];

        loop {
            if current_data.is_empty() {
                return Err(BencodeError::InvalidBencodeList);
            }
            if current_data[0] == b'e' {
                return Ok((Bencode::List(elements), &current_data[1..]));
            }

            let (element, rest) = Bencode::decode_recurisvely(current_data)?;
            elements.push(element);
            current_data = rest;
        }
    }

    fn decode_dictionary(data: &[u8]) -> Result<(Bencode, &[u8]), BencodeError> {
        let mut dict = BTreeMap::new();
        let mut current_data = &data[1..];

        loop {
            if current_data[0] == b'e' {
                return Ok((Bencode::Dict(dict), &current_data[1..]));
            }

            let (key, rest_after_key) = Bencode::decode_recurisvely(current_data)?;
            let key_bytes = match key {
                Bencode::Bytes(b) => b,
                _ => return Err(BencodeError::InvalidBencodeDict),
            };

            let (value, rest_after_value) = Bencode::decode_recurisvely(rest_after_key)?;

            dict.insert(key_bytes, value);
            current_data = rest_after_value;
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<&Bencode> {
        match self {
            Bencode::Dict(dict) => dict.get(key),
            _ => None,
        }
    }

    pub fn encode(bencode: &impl Encode) -> Vec<u8> {
        let bencode = bencode.to_bencode();
        Bencode::encoder(&bencode)
    }

    fn encoder(bencode: &Bencode) -> Vec<u8> {
        match bencode {
            Bencode::Int(i) => Bencode::encode_int(*i),
            Bencode::Bytes(bytes) => Bencode::encode_bytes(bytes),
            Bencode::List(list) => Bencode::encode_list(list),
            Bencode::Dict(dict) => Bencode::encode_dict(dict),
        }
    }

    fn encode_int(value: i64) -> Vec<u8> {
        let mut result = Vec::new();
        result.push(b'i');
        result.extend_from_slice(value.to_string().as_bytes());
        result.push(b'e');
        result
    }

    fn encode_bytes(bytes: &[u8]) -> Vec<u8> {
        let mut result = Vec::new();
        result.extend_from_slice(bytes.len().to_string().as_bytes());
        result.push(b':');
        result.extend_from_slice(bytes);
        result
    }

    fn encode_list(list: &[Bencode]) -> Vec<u8> {
        let mut result = Vec::new();
        result.push(b'l');
        for item in list {
            result.extend_from_slice(&Bencode::encoder(item));
        }
        result.push(b'e');
        result
    }

    fn encode_dict(dict: &BTreeMap<Vec<u8>, Bencode>) -> Vec<u8> {
        let mut result = Vec::new();
        result.push(b'd');
        for (key, value) in dict {
            result.extend_from_slice(&Bencode::encode_bytes(key));
            result.extend_from_slice(&Bencode::encoder(value));
        }
        result.push(b'e');
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bencode_decode_string() {
        let input = b"5:hello";
        let expected = Bencode::Bytes(b"hello".to_vec());
        let result = Bencode::decode(input).unwrap();
        dbg!(&result);
        dbg!(&expected);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_bencode_decode_integer() {
        let input = b"i5e";
        let expected = Bencode::Int(5);
        let result = Bencode::decode(input).unwrap();
        dbg!(&result);
        dbg!(&expected);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_bencode_decode_list() {
        let input = b"l5:helloi52ee";
        let expected = Bencode::List(vec![Bencode::Bytes(b"hello".to_vec()), Bencode::Int(52)]);
        let result = Bencode::decode(input).unwrap();
        dbg!(&result);
        dbg!(&expected);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_bencode_decode_dict() {
        let input = b"d3:foo3:bar5:helloi52ee";
        let expected = Bencode::Dict(
            vec![
                (b"foo".to_vec(), Bencode::Bytes(b"bar".to_vec())),
                (b"hello".to_vec(), Bencode::Int(52)),
            ]
            .into_iter()
            .collect(),
        );
        let result = Bencode::decode(input).unwrap();
        dbg!(&result);
        dbg!(&expected);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_bencode_enconde_string() {
        let input = Bencode::Bytes(b"hello".to_vec());
        let expected = b"5:hello".to_vec();
        let result = Bencode::encoder(&input);
        dbg!(&result);
        dbg!(&expected);
        assert_eq!(result, expected);
    }
}
