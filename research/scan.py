#!/usr/bin/env python3
"""Phase 0 research scanner: structural digest of a reference repo.
Pure local computation — no LLM calls. Usage: scan.py <repo> <outfile>"""
import os, sys, collections

repo, out = sys.argv[1], sys.argv[2]
SKIP_DIRS = {'.git','node_modules','target','dist','build','.venv','venv','__pycache__',
             '.pytest_cache','.mypy_cache','.ruff_cache','assets','website','coverage',
             '.next','.turbo','bazel-bin','bazel-out','bazel-*','vendor'}
SRC_EXT = {'.rs','.py','.ts','.tsx','.js','.jsx','.md','.sh','.toml','.json','.yaml','.yml',
           '.css','.html','.go','.rb','.mjs','.c','.h','.java','.kt','.nix','.bzl','.proto'}
BIN_EXT = {'.png','.jpg','.jpeg','.gif','.webp','.ico','.mp3','.wav','.mp4','.zip','.gz',
           '.woff','.woff2','.ttf','.icns','.bin','.o','.a','.so','.dylib','.wasm','.db','.jar'}

def walk(repo):
    for root, dirs, files in os.walk(repo):
        dirs[:] = [d for d in dirs if d not in SKIP_DIRS and not d.startswith('bazel-')]
        for f in files:
            yield os.path.join(root, f)

counts = {}   # dir -> (files, loc)
bigfiles = []  # (loc, path)
license_hdr = ''
total_loc = 0
total_files = 0
for path in walk(repo):
    rel = os.path.relpath(path, repo)
    ext = os.path.splitext(path)[1].lower()
    if ext in BIN_EXT:
        continue
    total_files += 1
    try:
        with open(path, 'r', errors='ignore') as fh:
            text = fh.read()
    except Exception:
        continue
    lines = text.count('\n')
    total_loc += lines
    top = rel.split(os.sep)[0] if os.sep in rel else '(root)'
    if top not in counts:
        counts[top] = [0, 0]
    counts[top][0] += 1
    counts[top][1] += lines
    if ext in SRC_EXT and lines > 30:
        bigfiles.append((lines, rel))
    if ext == '' and os.path.basename(path).upper() == 'LICENSE' and not license_hdr:
        license_hdr = text[:200].replace('\n', ' | ')

out_lines = []
out_lines.append(f"=== REPO: {os.path.basename(repo)} ===")
out_lines.append(f"text files: {total_files}  total LOC(text): {total_loc}")
if license_hdr:
    out_lines.append(f"LICENSE: {license_hdr}")
out_lines.append("\n-- LOC by top-level dir (files, loc) --")
for d, (fc, lc) in sorted(counts.items(), key=lambda x: -x[1][1]):
    out_lines.append(f"  {d:28s} {fc:6d} files  {lc:9d} loc")
out_lines.append("\n-- top 45 source files by LOC --")
for loc, rel in sorted(bigfiles, reverse=True)[:45]:
    out_lines.append(f"  {loc:7d}  {rel}")
with open(out, 'w') as fh:
    fh.write('\n'.join(out_lines) + '\n')
print(f"wrote {out}: {total_files} files, {total_loc} loc")
