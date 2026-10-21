#!/usr/bin/env python3
"""Inventory development Mach-O dependencies or bundle source-built FFmpeg."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys


def otool(flag, path):
    return subprocess.run(["/usr/bin/otool", flag, str(path)], check=True,
                          capture_output=True, text=True, timeout=30).stdout


def system(path):
    return os.path.normpath(path).startswith(("/usr/lib/", "/System/Library/"))


def inventory(executables, approve=None):
    nodes = {}
    edges = []
    unresolved = []
    visited = set()

    def expand(value, loader, executable):
        return value.replace("@loader_path", str(loader.parent)).replace("@executable_path", str(executable.parent))

    def visit(path, executable, inherited=()):
        path = path.resolve()
        if approve is not None:
            approve(path)
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


# No external codec libraries are licensed or supplied by the release builder.
FFMPEG_DYLIB = re.compile(r"lib(?:avcodec|avformat|avutil|swresample)(?:\.\d+)*\.dylib")


def within(path, directory):
    return path == directory or directory in path.parents


def rpaths(path):
    return re.findall(r"cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset",
                      otool("-l", path))


def mutate(arguments, path):
    subprocess.run(["/usr/bin/install_name_tool", *arguments, str(path)], check=True,
                   stdout=sys.stderr, stderr=sys.stderr, timeout=30)


def bundle(app, prefix):
    app = app.resolve(strict=True)
    prefix = prefix.resolve(strict=True)
    library_root = (prefix / "lib").resolve(strict=True)
    if not within(library_root, prefix) or not library_root.is_dir():
        raise ValueError(f"FFmpeg library directory escapes prefix: {library_root}")
    macos = app / "Contents/MacOS"
    frameworks = app / "Contents/Frameworks"
    resources = app / "Contents/Resources"
    for directory in (macos, frameworks, resources):
        if not within(directory.resolve(), app):
            raise ValueError(f"Bundle directory escapes app: {directory}")
    if not macos.is_dir():
        raise ValueError(f"Missing bundle executable directory: {macos}")

    executables = sorted(macos.iterdir())
    if not executables or any(not path.is_file() or path.is_symlink() for path in executables):
        raise ValueError("Contents/MacOS must contain regular executable files")
    existing = sorted(frameworks.iterdir()) if frameworks.exists() else []
    for path in existing:
        if not path.is_file() or path.is_symlink() or not FFMPEG_DYLIB.fullmatch(path.name):
            raise ValueError(f"Unapproved existing Frameworks entry: {path}")
    roots = executables + existing
    executable_set = set(executables)
    existing_set = set(existing)

    def approve_source(path):
        if path in executable_set or path in existing_set:
            return
        if not within(path, library_root) or not FFMPEG_DYLIB.fullmatch(path.name):
            raise ValueError(f"Unapproved non-system dependency (expected FFmpeg under {library_root}): {path}")

    original = inventory(roots, approve=approve_source)
    if original["unresolved"]:
        raise ValueError(f"Unresolved native dependencies: {original['unresolved']}")

    destinations = {}
    owners = {}
    for node in original["non_system_images"]:
        source = Path(node["path"])
        destination = source if source in executable_set or source in existing_set else frameworks / source.name
        previous = owners.get(destination)
        if previous is not None and previous != source:
            raise ValueError(f"Dylib basename collision: {previous} and {source}")
        if destination != source and destination.exists():
            raise ValueError(f"Dylib destination already exists: {destination}")
        owners[destination] = source
        destinations[source] = destination

    # Resolve the complete graph before touching the app. One load command cannot
    # safely mean different files in different executable/runpath contexts.
    changes = {}
    for edge in original["dependencies"]:
        source = Path(edge["from"])
        destination = destinations[source]
        if edge["system"]:
            replacement = os.path.normpath(edge["resolved"])
        else:
            target = destinations[Path(edge["resolved"]).resolve()]
            relative = os.path.relpath(target, destination.parent)
            replacement = "@loader_path/" + relative
        key = (destination, edge["install_name"])
        if key in changes and changes[key] != replacement:
            raise ValueError(f"Ambiguous dependency resolution: {key}")
        changes[key] = replacement

    frameworks.mkdir(parents=True, exist_ok=True)
    resources.mkdir(parents=True, exist_ok=True)
    for source, destination in destinations.items():
        if source != destination:
            shutil.copy2(source, destination)
            destination.chmod(destination.stat().st_mode | 0o200)
    for destination in sorted(owners):
        arguments = []
        if destination.parent == frameworks:
            arguments += ["-id", "@loader_path/" + destination.name]
        for (owner, name), replacement in sorted(changes.items()):
            if owner == destination and name != replacement:
                arguments += ["-change", name, replacement]
        # No runpaths are needed once every non-system edge is loader-relative.
        for value in dict.fromkeys(rpaths(destination)):
            arguments += ["-delete_rpath", value]
        if arguments:
            mutate(arguments, destination)

    def approve_bundled(path):
        if path not in owners:
            raise ValueError(f"Final dependency escapes the bundled image set: {path}")

    final = inventory(sorted(owners), approve=approve_bundled)
    if final["unresolved"]:
        raise ValueError(f"Unresolved bundled dependencies: {final['unresolved']}")
    for node in final["non_system_images"]:
        path = Path(node["path"])
        expected_ids = ["@loader_path/" + path.name] if path.parent == frameworks else []
        if node["install_names"] != expected_ids or node["rpaths"]:
            raise ValueError(f"Nonportable install ID or runpath: {path}")
    for edge in final["dependencies"]:
        path = Path(edge["from"])
        if edge["system"]:
            if not system(edge["install_name"]):
                raise ValueError(f"Indirect system dependency: {edge}")
        else:
            target = Path(edge["resolved"]).resolve()
            expected = "@loader_path/" + os.path.relpath(target, path.parent)
            if edge["install_name"] != expected:
                raise ValueError(f"Nonportable bundled dependency: {edge}")

    # Validate before signing: install_name_tool invalidates existing signatures.
    for path in sorted(owners):
        if path.parent == frameworks:
            subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", str(path)],
                           check=True, stdout=sys.stderr, stderr=sys.stderr, timeout=30)

    def relative(path):
        return Path(path).relative_to(app).as_posix()

    report = {
        "schema_version": 1,
        "executables": [relative(path) for path in executables],
        "non_system_images": [
            {**node, "path": relative(node["path"])}
            for node in final["non_system_images"]
        ],
        "dependencies": sorted([
            {**edge, "from": relative(edge["from"]),
             "resolved": os.path.normpath(edge["resolved"]) if edge["system"]
             else relative(Path(edge["resolved"]).resolve())}
            for edge in final["dependencies"]
        ], key=lambda edge: (edge["from"], edge["install_name"], edge["resolved"])),
        "unresolved": [],
        "limitations": [
            "System images may reside only in the dyld shared cache.",
            "Inventory follows Mach-O load commands, not optional runtime dlopen/plugin paths.",
        ],
    }
    output = resources / "native-dependencies.json"
    output.write_text(json.dumps(report, indent=2) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output")
    parser.add_argument("--bundle", type=Path)
    parser.add_argument("--prefix", type=Path)
    parser.add_argument("binaries", nargs="*")
    args = parser.parse_args()
    if args.bundle is not None:
        if args.prefix is None or args.output is not None or args.binaries:
            parser.error("--bundle requires --prefix and cannot be combined with --output or binaries")
        try:
            report = bundle(args.bundle, args.prefix)
        except (ValueError, OSError, subprocess.SubprocessError) as error:
            raise SystemExit(f"Native dependency bundling failed: {error}") from error
        print(f"Bundled native dependencies: {len(report['non_system_images'])} images", file=sys.stderr)
        return
    if args.prefix is not None or args.output is None or not args.binaries:
        parser.error("inventory requires --output and one or more binaries")
    report = inventory([Path(value) for value in args.binaries])
    Path(args.output).write_text(json.dumps(report, indent=2) + "\n")
    if report["unresolved"]:
        raise SystemExit(f"Missing native dependencies; inspect {args.output}. Install development prerequisites with brew install ffmpeg pkg-config and rebuild.")
    print(f"Native dependency inventory: {args.output} ({len(report['non_system_images'])} non-system images)")


if __name__ == "__main__":
    main()
