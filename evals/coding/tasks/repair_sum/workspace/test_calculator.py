import unittest

from calculator import add


class CalculatorTests(unittest.TestCase):
    def test_adds_positive_and_negative_values(self) -> None:
        self.assertEqual(add(2, 3), 5)
        self.assertEqual(add(-2, 1), -1)


if __name__ == "__main__":
    unittest.main()

