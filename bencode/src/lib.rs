pub mod bencode;

#[cfg(test)]
mod tests {
    use crate::bencode::Bencode;

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
