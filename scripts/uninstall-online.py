"""Remove unchanged registered K3 files, keeping user data and edited files."""
import argparse
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from installation_state import uninstall, uninstall_plan

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--prefix', type=Path, required=True)
parser.add_argument('--plan', action='store_true')
parser.add_argument('--yes', action='store_true')
args = parser.parse_args()
try:
    if args.plan:
        print(json.dumps(uninstall_plan(args.prefix.absolute()), ensure_ascii=True))
    elif not args.yes and input(f'Remove K3 installation files from {args.prefix}? [y/N]: ').lower() not in {'y', 'yes'}:
        print('Cancelled. No installation files were removed.')
    else:
        uninstall(args.prefix.absolute())
        print('Uninstall complete. Personal projects, recordings and settings were preserved.')
except (OSError, ValueError, RuntimeError) as error:
    print(f'Uninstall failed: {error}', file=sys.stderr)
    sys.exit(1)
