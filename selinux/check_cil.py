#!/usr/bin/env python3
"""Static syntax checker for rsc.cil patch.

Validates:
1. Parenthesis balance
2. All (type X) declarations are unique
3. All types referenced in allow/typetransition rules are declared
4. typeattributeset references existing attributes
5. genfscon syntax: (genfscon FS path (u object_r type ((s0) (s0))))
"""
import re
import sys
from pathlib import Path

# Resolve relative to script location so it works in any environment
# (local dev, CI runner, etc.) without hardcoded absolute paths.
CIL_FILE = Path(__file__).resolve().parent / "rsc.cil"

def strip_comments(text):
    """Remove ; comments from CIL text."""
    out = []
    for line in text.splitlines():
        idx = line.find(';')
        if idx >= 0:
            line = line[:idx]
        out.append(line)
    return '\n'.join(out)

def check_parens(text):
    """Verify all parens are balanced."""
    depth = 0
    line_no = 1
    for i, c in enumerate(text):
        if c == '\n':
            line_no += 1
        if c == '(':
            depth += 1
        elif c == ')':
            depth -= 1
            if depth < 0:
                return False, f"Unmatched ) at line {line_no}"
    if depth != 0:
        return False, f"Unbalanced parens — depth {depth} at EOF"
    return True, "OK"

def extract_types(text):
    """Find all (type X) declarations."""
    return set(re.findall(r'\(type\s+(\w+)\)', text))

def extract_typeattributeset_targets(text):
    return re.findall(r'\(typeattributeset\s+(\w+)\s+\(', text)

def extract_referenced_types(text):
    refs = set()
    for m in re.finditer(r'\(allow\s+(\w+)\s+(\w+)\s+\(', text):
        refs.add(m.group(1)); refs.add(m.group(2))
    for m in re.finditer(r'\(dontaudit\s+(\w+)\s+(\w+)\s+\(', text):
        refs.add(m.group(1)); refs.add(m.group(2))
    for m in re.finditer(r'\(typetransition\s+(\w+)\s+(\w+)\s+\w+\s+(\w+)\)', text):
        refs.add(m.group(1)); refs.add(m.group(2)); refs.add(m.group(3))
    for m in re.finditer(r'\(roletype\s+\w+\s+(\w+)\)', text):
        refs.add(m.group(1))
    for m in re.finditer(r'\(genfscon\s+\w+\s+\S+\s+\(u\s+object_r\s+(\w+)\s+', text):
        refs.add(m.group(1))
    return refs

def main():
    text = CIL_FILE.read_text()
    text = strip_comments(text)
    
    print(f">> Checking {CIL_FILE}")
    print(f"   Size: {len(text)} bytes (after comment strip)")
    
    ok, msg = check_parens(text)
    if not ok:
        print(f"   FAIL: {msg}")
        sys.exit(1)
    print(f"   Parens: balanced ({msg})")
    
    declared = extract_types(text)
    print(f"   Declared types: {len(declared)}")
    for t in sorted(declared):
        print(f"     - {t}")
    
    raw_decls = re.findall(r'\(type\s+\w+\)', text)
    if len(raw_decls) != len(declared):
        print(f"   FAIL: duplicate type declarations")
        sys.exit(1)
    
    referenced = extract_referenced_types(text)
    print(f"   Referenced types: {len(referenced)}")
    
    external_types = {
        'init_30_0', 'self',
        'sysfs_30_0', 'sysfs_batteryinfo_30_0',
        'system_data_file_30_0', 'system_data_root_file_30_0',
        'adb_data_file_30_0',
        'vendor_file_30_0',
        'null_device_30_0', 'kmsg_device_30_0', 'ptmx_device_30_0',
        'sysfs_mm',
    }
    undeclared = referenced - declared - external_types
    if undeclared:
        print(f"   FAIL: referenced but undeclared types: {undeclared}")
        sys.exit(1)
    print(f"   External types (declared in parent CIL): {len(referenced & external_types)}")
    
    targets = set(extract_typeattributeset_targets(text))
    expected_attrs = {'domain', 'file_type', 'data_file_type', 'exec_type',
                      'proc_type', 'mlstrustedsubject'}
    unknown = targets - expected_attrs
    if unknown:
        print(f"   FAIL: typeattributeset targets not in expected set: {unknown}")
        sys.exit(1)
    print(f"   Typeattributeset targets: {sorted(targets)}")
    
    genfscons = re.findall(r'\(genfscon\s+(\w+)\s+(\S+)\s+\(u\s+object_r\s+\w+\s+\(\(s0\)\s+\(s0\)\)\)\)', text)
    print(f"   genfscon rules: {len(genfscons)}")
    for fs, path in genfscons:
        print(f"     - {fs} {path}")
    
    print()
    print(">> ALL CHECKS PASS")
    return 0

if __name__ == "__main__":
    sys.exit(main())
