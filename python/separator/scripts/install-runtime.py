#!/usr/bin/env python3
"""Select and install PyTorch before the worker's other dependencies are installed."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from k3_separator.setup_runtime import main

if __name__ == "__main__":
    main()
