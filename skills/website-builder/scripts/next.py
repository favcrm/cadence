"""Create the run if absent and print the next step as one JSON object.

Usage: python3 scripts/next.py --run DIR
"""

import argparse
import os

from common import emit, load, new_state, now, run, save, state_path, view


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--run", required=True, metavar="DIR")
    args = parser.parse_args()
    if os.path.isfile(state_path(args.run)):
        state = load(args.run)
    else:
        os.makedirs(args.run, exist_ok=True)
        stamp = now()
        state = new_state(args.run, stamp)
        save(args.run, state, stamp)
    emit(view(state))
    return 0


if __name__ == "__main__":
    raise SystemExit(run(main))
