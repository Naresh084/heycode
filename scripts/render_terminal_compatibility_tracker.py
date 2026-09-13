#!/usr/bin/env python3
"""Render the Phase 2 requirement table from its authoritative JSON data."""
import json
from collections import Counter
from pathlib import Path

root = Path(__file__).resolve().parent.parent
data = json.loads((root / 'docs/terminal-compatibility-tracker.json').read_text())
counts = Counter(item['status'] for item in data['items'])

def cell(value):
    return str(value).replace('|', '\\|').replace('\n', '<br>')

lines = [
    '# Phase 2 implementation tracker', '',
    'Source: [complete report](terminal-compatibility-requirements.md). Machine-readable requirements and full report-block coverage: [tracker data](terminal-compatibility-tracker.json).', '',
    'Status: active. A requirement is complete only with implementation and its required verification evidence. Existing code is not assumed complete. External service requirements remain open if they cannot be exercised. Deferred/experimental exclusions are constraints to enforce, not features to silently enable.', '',
    f"Current counts: **{counts['complete']} complete**, **{counts['validating']} validating**, **{counts['in_progress']} in progress**, {counts['deferred']} deferred, and {counts['not_applicable']} not applicable.", '',
    'Evidence is chronological; later verified entries supersede earlier pending-state notes.', '',
    '| ID | Requirement | Status | Evidence |',
    '|---|---|---|---|',
]
for item in data['items']:
    row = [item['id'], item['title'], item['status'], '; '.join(item['evidence']) or '—']
    lines.append('| ' + ' | '.join(map(cell, row)) + ' |')
(root / 'docs/terminal-compatibility-tracker.md').write_text('\n'.join(lines) + '\n')
print(dict(counts))
