import unittest

from parser import parse_pairs


class ParserTests(unittest.TestCase):
    def test_parses_and_trims_pairs(self) -> None:
        self.assertEqual(parse_pairs(" a = 1,b=two=parts "), {"a": "1", "b": "two=parts"})

    def test_rejects_malformed_and_duplicate_keys(self) -> None:
        for value in ("missing", "=blank", "a=1,a=2"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                parse_pairs(value)


if __name__ == "__main__":
    unittest.main()
