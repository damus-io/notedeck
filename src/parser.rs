use log::info;

struct Parser<'a> {
    data: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(data: &'a str) -> Parser {
        Parser { data: data, pos: 0 }
    }

    fn parse_until(&mut self, needle: char) -> bool {
        let mut count = 0;
        for c in self.data[self.pos..].chars() {
            if c == needle {
                self.pos += count - 1;
                return true;
            } else {
                count += 1;
            }
        }

        return false;
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parser() {
        let s = "hey there #hashtag";
        let mut parser = Parser::new(s);
        parser.parse_until('#');
        assert_eq!(parser.pos, 9);
        parser.parse_until('t');
        assert_eq!(parser.pos, 14);
    }
}
