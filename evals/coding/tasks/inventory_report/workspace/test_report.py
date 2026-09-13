import unittest

from report import render_report


class ReportTests(unittest.TestCase):
    def test_normalizes_counts_and_sorts_rows(self) -> None:
        self.assertEqual(
            render_report([" Pear ", "apple", "PEAR", "red   grape"]),
            "apple: 1\npear: 2\nred grape: 1",
        )


if __name__ == "__main__":
    unittest.main()

