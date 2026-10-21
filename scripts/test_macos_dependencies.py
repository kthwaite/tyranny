#!/usr/bin/env python3
"""Exercise the release bundler against real, tiny Mach-O dependency graphs."""
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("macos_dependencies.py")


@unittest.skipUnless(sys.platform == "darwin" and shutil.which("clang"),
                     "requires macOS and Xcode command-line tools")
class BundleTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tyranny-native-tests-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.prefix = self.root / "native source é"
        self.lib = self.prefix / "lib"
        self.lib.mkdir(parents=True)
        self.app = self.root / "Tyranny.app"
        self.macos = self.app / "Contents/MacOS"
        self.macos.mkdir(parents=True)
        self.sequence = 0

    def compile(self, output, body, dependencies=(), identity=None, runpaths=()):
        self.sequence += 1
        source = self.root / f"fixture-{self.sequence}.c"
        source.write_text(body)
        output.parent.mkdir(parents=True, exist_ok=True)
        command = ["clang", "-Wl,-headerpad_max_install_names"]
        if identity is not None:
            command += ["-dynamiclib", "-Xlinker", "-install_name", "-Xlinker", identity]
        for runpath in runpaths:
            command += ["-Xlinker", "-rpath", "-Xlinker", str(runpath)]
        command += [str(source), *(str(path) for path in dependencies), "-o", str(output)]
        subprocess.run(command, check=True, capture_output=True, text=True, timeout=60)
        return output

    def chain(self):
        utility = self.compile(self.lib / "libavutil.61.dylib", "int utility(void) { return 7; }",
                               identity="@rpath/libavutil.61.dylib")
        codec = self.compile(self.lib / "libavcodec.63.dylib",
                             "int utility(void); int codec(void) { return utility() + 1; }",
                             [utility], identity="@rpath/libavcodec.63.dylib")
        self.compile(self.macos / "tyranny-server",
                     "int codec(void); int main(void) { return codec() == 8 ? 0 : 1; }",
                     [codec], runpaths=[self.lib])
        return utility, codec

    def invoke(self, success=True):
        result = subprocess.run([sys.executable, str(SCRIPT), "--bundle", str(self.app),
                                 "--prefix", str(self.prefix)],
                                capture_output=True, text=True, timeout=60)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stderr)
        return result

    def test_transitive_closure_survives_source_removal_and_app_move(self):
        self.chain()
        self.invoke()
        report_path = self.app / "Contents/Resources/native-dependencies.json"
        serialized = report_path.read_text()
        report = json.loads(serialized)
        self.assertNotIn(str(self.root), serialized)
        self.assertEqual(report["unresolved"], [])
        self.assertEqual([node["path"] for node in report["non_system_images"]], [
            "Contents/Frameworks/libavcodec.63.dylib",
            "Contents/Frameworks/libavutil.61.dylib",
            "Contents/MacOS/tyranny-server",
        ])
        links = {(edge["from"], edge["install_name"]) for edge in report["dependencies"]
                 if not edge["system"]}
        self.assertEqual(links, {
            ("Contents/MacOS/tyranny-server", "@loader_path/../Frameworks/libavcodec.63.dylib"),
            ("Contents/Frameworks/libavcodec.63.dylib", "@loader_path/libavutil.61.dylib"),
        })
        self.assertTrue(all(not node["rpaths"] for node in report["non_system_images"]))
        self.invoke()
        self.assertEqual(report_path.read_text(), serialized)
        # Signing the executables is the caller's responsibility, after relocation.
        subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-",
                        str(self.macos / "tyranny-server")], check=True, capture_output=True)
        shutil.rmtree(self.prefix)
        relocated = self.root / "moved app é/Tyranny.app"
        relocated.parent.mkdir()
        self.app.rename(relocated)
        subprocess.run([str(relocated / "Contents/MacOS/tyranny-server")], check=True,
                       env={"PATH": "/usr/bin:/bin"}, capture_output=True, timeout=10)

    def test_rebuilding_executable_replaces_previous_pinned_library(self):
        utility, codec = self.chain()
        self.invoke()
        self.compile(utility, "int utility(void) { return 17; }",
                     identity="@rpath/libavutil.61.dylib")
        self.compile(self.macos / "tyranny-server",
                     "int codec(void); int main(void) { return codec() == 18 ? 0 : 1; }",
                     [codec], runpaths=[self.lib])
        self.invoke()
        subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-",
                        str(self.macos / "tyranny-server")], check=True, capture_output=True)
        shutil.rmtree(self.prefix)
        subprocess.run([str(self.macos / "tyranny-server")], check=True,
                       env={"PATH": "/usr/bin:/bin"}, capture_output=True, timeout=10)

    def test_tracker_library_closure_uses_only_system_cpp_runtime(self):
        tracker = self.compile(
            self.lib / "libopenmpt.0.dylib", "int tracker(void) { return 9; }",
            ["-lc++"], identity=str(self.lib / "libopenmpt.0.dylib"))
        demuxer = self.compile(
            self.lib / "libavformat.63.dylib",
            "int tracker(void); int demuxer(void) { return tracker(); }",
            [tracker], identity=str(self.lib / "libavformat.63.dylib"))
        self.compile(self.macos / "tyranny-server",
                     "int demuxer(void); int main(void) { return demuxer() == 9 ? 0 : 1; }",
                     [demuxer])
        self.invoke()
        report = json.loads((self.app / "Contents/Resources/native-dependencies.json").read_text())
        self.assertEqual(report["unresolved"], [])
        self.assertEqual([node["path"] for node in report["non_system_images"]], [
            "Contents/Frameworks/libavformat.63.dylib",
            "Contents/Frameworks/libopenmpt.0.dylib",
            "Contents/MacOS/tyranny-server",
        ])
        self.assertTrue(any(edge["system"] and edge["install_name"] == "/usr/lib/libc++.1.dylib"
                            for edge in report["dependencies"]))
        subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-",
                        str(self.macos / "tyranny-server")], check=True, capture_output=True)
        shutil.rmtree(self.prefix)
        relocated = self.root / "moved tracker app/Tyranny.app"
        relocated.parent.mkdir()
        self.app.rename(relocated)
        subprocess.run([str(relocated / "Contents/MacOS/tyranny-server")], check=True,
                       env={"PATH": "/usr/bin:/bin"}, capture_output=True, timeout=10)

    def test_rejects_non_system_cpp_runtime(self):
        runtime = self.compile(self.lib / "libc++.1.dylib",
                               "int runtime(void) { return 0; }",
                               identity=str(self.lib / "libc++.1.dylib"))
        tracker = self.compile(self.lib / "libopenmpt.0.dylib",
                               "int runtime(void); int tracker(void) { return runtime(); }",
                               [runtime], identity=str(self.lib / "libopenmpt.0.dylib"))
        self.compile(self.macos / "tyranny-server",
                     "int tracker(void); int main(void) { return tracker(); }", [tracker])
        self.assertIn("Unapproved non-system dependency", self.invoke(success=False).stderr)
        self.assertFalse((self.app / "Contents/Frameworks").exists())

    def test_rejects_missing_transitive_dependency(self):
        utility, _ = self.chain()
        utility.unlink()
        result = self.invoke(success=False)
        self.assertIn("Unresolved native dependencies", result.stderr)
        self.assertFalse((self.app / "Contents/Frameworks").exists())

    def test_rejects_external_library_even_with_approved_basename(self):
        external = self.root / "outside/libavutil.61.dylib"
        self.compile(external, "int utility(void) { return 0; }", identity=str(external))
        self.compile(self.macos / "tyranny-server",
                     "int utility(void); int main(void) { return utility(); }", [external])
        self.assertIn("Unapproved non-system dependency", self.invoke(success=False).stderr)
        self.assertFalse((self.app / "Contents/Frameworks").exists())

    def test_rejects_prefix_symlink_escape(self):
        external = self.root / "outside/libavutil.61.dylib"
        link = self.lib / external.name
        self.compile(external, "int utility(void) { return 0; }", identity=str(link))
        link.symlink_to(external)
        self.compile(self.macos / "tyranny-server",
                     "int utility(void); int main(void) { return utility(); }", [link])
        self.assertIn("Unapproved non-system dependency", self.invoke(success=False).stderr)

    def test_rejects_unapproved_library_inside_prefix(self):
        external = self.lib / "libexternal.dylib"
        self.compile(external, "int external(void) { return 0; }", identity=str(external))
        self.compile(self.macos / "tyranny-server",
                     "int external(void); int main(void) { return external(); }", [external])
        self.assertIn("Unapproved non-system dependency", self.invoke(success=False).stderr)

    def test_rejects_distinct_sources_with_same_basename(self):
        first = self.lib / "one/libavutil.61.dylib"
        second = self.lib / "two/libavutil.61.dylib"
        self.compile(first, "int first(void) { return 0; }", identity=str(first))
        self.compile(second, "int second(void) { return 0; }", identity=str(second))
        self.compile(self.macos / "tyranny-server",
                     "int first(void); int second(void); int main(void) { return first() + second(); }",
                     [first, second])
        self.assertIn("Dylib basename collision", self.invoke(success=False).stderr)
        self.assertFalse((self.app / "Contents/Frameworks").exists())


if __name__ == "__main__":
    unittest.main()
