#!/usr/bin/env python3
"""Print a progress report for the L1→L3 upgrade. Designed to be called repeatedly by a monitor."""

import re
import sys
from datetime import datetime

LOG = "l1_to_l3_progress.log"

def main():
    lines = open(LOG).readlines()
    completed = sum(1 for l in lines if '-> OK:' in l or '-> inserted' in l)

    entries = []
    i = 0
    while i < len(lines):
        m = re.search(r'\[(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})\].*Researching: (.+) \(ID (\d+)\)', lines[i])
        if m and i + 1 < len(lines):
            start = datetime.strptime(m.group(1), '%Y-%m-%d %H:%M:%S')
            name = m.group(2)
            obj_id = m.group(3)
            m2 = re.search(r'\[(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})\].*?(\d+) research chars', lines[i + 1])
            if m2:
                end = datetime.strptime(m2.group(1), '%Y-%m-%d %H:%M:%S')
                chars = int(m2.group(2))
                elapsed = int((end - start).total_seconds())
                entries.append((name, obj_id, chars, elapsed))
                i += 2
                continue
        i += 1

    avg_time = sum(e for _, _, _, e in entries) / len(entries) if entries else 0
    remaining = 1728 - completed
    eta_hours = (remaining * avg_time) / 3600
    fail_count = sum(1 for _, _, c, _ in entries if c == 0)

    now = datetime.now().strftime("%H:%M")
    print(f"=== [{now}] Completed: {completed}/1728 ({completed * 100 // 1728}%) | Failures: {fail_count} | Avg: {avg_time // 60:.0f}m{avg_time % 60:02.0f}s | ETA: {eta_hours:.0f}h ===")
    print()

    # Last 10 completed
    recent = entries[-10:]
    if recent:
        print(f"| {'Artwork':<55} | {'ID':>6} | {'Chars':>6} | {'Time':>6} |")
        print(f"|{'-' * 57}|{'-' * 8}|{'-' * 8}|{'-' * 8}|")
        for name, oid, chars, elapsed in recent:
            dname = (name[:53] + '..') if len(name) > 55 else name
            flag = ' FAIL' if chars == 0 else ''
            print(f"| {dname:<55} | {oid:>6} | {chars:>6} | {elapsed // 60}m{elapsed % 60:02d}s |{flag}")
    print()

    # Current
    for line in lines[-1:]:
        l = line.strip()
        if 'Researching' in l:
            part = l.split("Researching: ", 1)[1]
            print(f"Now: {part}")


if __name__ == "__main__":
    main()
