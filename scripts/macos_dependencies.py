#!/usr/bin/env python3
"""Inventory actual development Mach-O dependencies, not distribution clearance."""
import argparse
import json
from pathlib import Path
import re
import subprocess


def otool(flag, path):
    return subprocess.run(["/usr/bin/otool", flag, str(path)], check=True,
                          capture_output=True, text=True, timeout=30).stdout


def system(path):
    return path.startswith(("/usr/lib/", "/System/Library/"))


def inventory(executables):
    nodes = {}
    edges = []
    unresolved = []
    visited = set()

    def expand(value, loader, executable):
        return value.replace("@loader_path", str(loader.parent)).replace("@executable_path", str(executable.parent))

    def visit(path, executable, inherited=()):
        path = path.resolve()
        context = (str(path), str(executable), inherited)
        if context in visited:
            return
        visited.add(context)
        load_commands = otool("-l", path)
        own_rpaths = tuple(expand(value, path, executable) for value in re.findall(
            r"cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset", load_commands))
        rpaths = tuple(dict.fromkeys(own_rpaths + inherited))
        identity_lines = otool("-D", path).splitlines()[1:]
        identities = {line.strip() for line in identity_lines if line.strip().startswith(("/", "@"))}
        links = re.findall(r"^\s+(.+?) \(compatibility version (.*)\)$", otool("-L", path), re.MULTILINE)
        nodes[str(path)] = {"path": str(path), "rpaths": list(own_rpaths), "install_names": sorted(identities)}
        for name, versions in links:
            if name in identities:
                continue
            expanded = expand(name, path, executable)
            candidates = [str(Path(base) / name[7:]) for base in rpaths] if name.startswith("@rpath/") else [expanded]
            resolved = next((candidate for candidate in candidates if system(candidate) or (candidate.startswith("/") and Path(candidate).is_file())), None)
            edge = {"from": str(path), "install_name": name, "versions": versions,
                    "resolved": resolved, "system": bool(resolved and system(resolved))}
            if edge not in edges:
                edges.append(edge)
            if resolved is None:
                if edge not in unresolved:
                    unresolved.append(edge)
            elif not system(resolved):
                target = Path(resolved).resolve()
                # Cycles cannot introduce new inherited runpaths indefinitely.
                if target != path:
                    visit(target, executable, rpaths)

    for binary in executables:
        visit(binary.resolve(), binary.resolve())
    return {"executables": [str(path.resolve()) for path in executables],
            "non_system_images": sorted(nodes.values(), key=lambda node: node["path"]),
            "dependencies": edges, "unresolved": unresolved,
            "limitations": ["Development bundle retains Homebrew/non-system dynamic links; it is not relocatable.",
                            "System images may reside only in the dyld shared cache.",
                            "Inventory follows Mach-O load commands, not optional runtime dlopen/plugin paths.",
                            "No redistribution or codec-license clearance is asserted."]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True)
    parser.add_argument("binaries", nargs="+")
    args = parser.parse_args()
    report = inventory([Path(value) for value in args.binaries])
    Path(args.output).write_text(json.dumps(report, indent=2) + "\n")
    if report["unresolved"]:
        raise SystemExit(f"Missing native dependencies; inspect {args.output}. Install development prerequisites with brew install ffmpeg pkg-config and rebuild.")
    print(f"Native dependency inventory: {args.output} ({len(report['non_system_images'])} non-system images)")


if __name__ == "__main__":
    main()
