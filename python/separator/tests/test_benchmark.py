import unittest

from k3_separator.benchmark import recommend_concurrency


class BenchmarkTests(unittest.TestCase):
    def test_recommends_highest_concurrency_without_a_failure(self) -> None:
        records = [
            {"concurrency": 1, "failures": 0},
            {"concurrency": 2, "failures": 0},
            {"concurrency": 3, "failures": 1},
        ]

        self.assertEqual(2, recommend_concurrency(records))


if __name__ == "__main__":
    unittest.main()
