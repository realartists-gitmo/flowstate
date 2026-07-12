#!/usr/bin/env bash
set -e
cd /home/adam/Projects/flowstate
# merge existing eval raws + focused silent raws, dedup by id (focus wins)
python3 -c "
import json
seen={}
for f in ['scratchpad_raws.jsonl','scratchpad_raws_focus.jsonl']:
    for l in open(f):
        if not l.strip(): continue
        r=json.loads(l); seen[r['id']]=l.rstrip('\n')
open('scratchpad_raws_merged.jsonl','w').write('\n'.join(seen.values())+'\n')
print('merged', len(seen))
"
echo '=== BASELINE ==='
/tmp/claude-1000/replay_base scratchpad_raws_merged.jsonl scratchpad_heldout.jsonl --out scratchpad_base_scored.jsonl | grep -E 'SILENT|precision'
echo '=== NEW (all fixes) ==='
./target/release/replay scratchpad_raws_merged.jsonl scratchpad_heldout.jsonl --out scratchpad_replay_scored.jsonl | grep -E 'SILENT|precision'
echo '=== DIFF ==='
python3 scratchpad_diff.py scratchpad_base_scored.jsonl scratchpad_replay_scored.jsonl
