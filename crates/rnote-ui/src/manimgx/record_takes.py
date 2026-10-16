"""Record every ManimGX scene of a file as a take, for Rnote.

    python record_takes.py SCENE_FILE OUT_DIR [KNOWN_HASHES]

Writes OUT_DIR/NNNN_<Scene>.take for each slide of the file, in order; each becomes one page
in Rnote. Beside each take, NNNN_<Scene>.hash holds the hash of what made it (`slide_hash`).
KNOWN_HASHES, a JSON object {scene: hash}, names takes Rnote already has: a slide whose hash
is unchanged is skipped. The slides are the scene classes listed in the file's `SLIDES`, or, without it,
every scene class the file defines (except names with a leading underscore), in the order of
the file. Rnote runs this with a Python that has ManimGX and shows the progress it prints,
one line each:

    scenes 3                the number of scenes
    scene 2 Doppelspalt     scene 2 begins
    play 7 12.5             its play 7 ended, 12.5 s into the scene
    skip 3 Euler            scene 3 is unchanged, not recorded

Cues: an object whose `rnote_cue` attribute is true is for the lecturer only. Its records carry
the CUE flag; Rnote draws them on the lecturer's screen, not in the presentation window. A
scene file marks them itself, e.g.

    def cue(mob):
        for part in mob.get_family():
            part.rnote_cue = True
        return mob
"""

import ast
import hashlib
import importlib.metadata
import importlib.util
import inspect
import json
import struct
import sys
from pathlib import Path

import manimgx as m
import manimgx.rendering.feed as feed

#: Frames per second of the takes. Rnote blends between frames, so 30 plays smoothly and
#: keeps documents small.
FPS = 30

#: The attribute that marks an object as a cue (see above).
CUE_ATTRIBUTE = "rnote_cue"

#: Record flag of a cue: a bit ManimGX does not use (its flags end at bit 9).
CUE = 1 << 32

#: Byte offset of a record's flags: its eighth u64 (ManimGX `render::Record`).
FLAGS_AT = 7 * 8


def scenes_of(module) -> list[type]:
    """The slides of `module`: its `SLIDES`, else its scene classes in the order of the file."""
    if hasattr(module, "SLIDES"):
        slides = list(module.SLIDES)
        for slide in slides:
            if not (inspect.isclass(slide) and issubclass(slide, m.Scene)):
                raise TypeError(f"SLIDES holds {slide!r}, not a scene class")
        return slides

    found = [
        cls
        for name, cls in vars(module).items()
        if inspect.isclass(cls)
        and issubclass(cls, m.Scene)
        and cls.__module__ == module.__name__
        and not name.startswith("_")
    ]
    return sorted(found, key=lambda cls: inspect.getsourcelines(cls)[1])


#: The list of slides; its names only order the slides, so a slide does not depend on the others.
SLIDES_NAME = "SLIDES"


def slide_hash(scene_file: Path, name: str) -> str:
    """Hash of what makes slide `name`: its class, the module-level definitions it names (and
    those they name, and so on), every other module-level statement (settings, imports,
    `SLIDES`), modules imported from the scene's folder, ManimGX's version and this driver.

    Example: editing `Euler` changes Euler's hash only; editing a helper `title` changes the
    hash of every slide that calls it; reordering `SLIDES` changes all (the slide numbers).
    """
    source = scene_file.read_text(encoding="utf-8")
    tree = ast.parse(source)

    # Definitions by name; anything else counts for every slide.
    defined: dict[str, list[ast.stmt]] = {}
    common: list[ast.stmt] = []
    for node in tree.body:
        names = defined_names(node)
        if not names or SLIDES_NAME in names:
            common.append(node)
            continue
        for defined_name in names:
            defined.setdefault(defined_name, []).append(node)

    # The slide's definitions, following the names they use.
    needed, todo = set(), [name]
    while todo:
        current = todo.pop()
        if current in needed or current not in defined:
            continue
        needed.add(current)
        for node in defined[current]:
            todo += [n.id for n in ast.walk(node) if isinstance(n, ast.Name)]

    nodes = {id(n): n for n in common + [n for d in sorted(needed) for n in defined[d]]}
    in_order = sorted(nodes.values(), key=lambda n: n.lineno)
    parts = [ast.get_source_segment(source, n) or "" for n in in_order]
    parts += [local_module_source(scene_file.parent, n) for n in common]
    parts += [importlib.metadata.version("manimgx"), Path(__file__).read_text(encoding="utf-8")]
    return hashlib.sha256("\0".join(parts).encode("utf-8")).hexdigest()


def defined_names(node: ast.stmt) -> list[str]:
    """Names a module-level statement defines: a function, class or plain assignment."""
    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
        return [node.name]
    if isinstance(node, ast.Assign):
        return [t.id for t in node.targets if isinstance(t, ast.Name)]
    if isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
        return [node.target.id]
    return []


def local_module_source(folder: Path, node: ast.stmt) -> str:
    """The source of the modules from `folder` that an import statement imports."""
    if isinstance(node, ast.Import):
        modules = [alias.name for alias in node.names]
    elif isinstance(node, ast.ImportFrom) and node.module and not node.level:
        modules = [node.module]
    else:
        return ""
    files = [folder / f"{module.replace('.', '/')}.py" for module in modules]
    return "".join(f.read_text(encoding="utf-8") for f in files if f.is_file())


def mark_cues() -> None:
    """Make ManimGX flag the records of cue objects.

    Patches `Feeder.record`, which builds one object's record (pinned ManimGX 0.1.1).
    """
    feeder = feed.Feeder
    record = feeder.record

    def flagged(self, mob, *args, **kwargs):
        data = record(self, mob, *args, **kwargs)
        if not data or not getattr(mob, CUE_ATTRIBUTE, False):
            return data

        out = bytearray(data)
        (flags,) = struct.unpack_from("<Q", out, FLAGS_AT)
        struct.pack_into("<Q", out, FLAGS_AT, flags | CUE)
        return bytes(out)

    feeder.record = flagged


def main() -> None:
    scene_file, out = Path(sys.argv[1]).resolve(), Path(sys.argv[2])
    known = json.loads(Path(sys.argv[3]).read_text(encoding="utf-8")) if len(sys.argv) > 3 else {}

    # The scene's folder first, so its own modules (a style kit) import.
    sys.path.insert(0, str(scene_file.parent))
    spec = importlib.util.spec_from_file_location(scene_file.stem, scene_file)
    module = importlib.util.module_from_spec(spec)
    sys.modules[scene_file.stem] = module
    spec.loader.exec_module(module)

    m.config.frame_rate = FPS
    mark_cues()
    scenes = scenes_of(module)
    report(f"scenes {len(scenes)}")
    for index, scene in enumerate(scenes):
        digest = slide_hash(scene_file, scene.__name__)
        if known.get(scene.__name__) == digest:
            report(f"skip {index + 1} {scene.__name__}")
            continue

        report(f"scene {index + 1} {scene.__name__}")
        chunks: list[bytes] = []
        scene().render(
            take=chunks.append,
            plays=lambda play, _: report(f"play {play.index + 1} {float(play.end):.1f}"),
        )
        stem = out / f"{index:04d}_{scene.__name__}"
        stem.with_suffix(".take").write_bytes(b"".join(chunks))
        stem.with_suffix(".hash").write_text(digest, encoding="utf-8")


def report(line: str) -> None:
    """One line of progress, at once: Rnote shows it while the scenes run."""
    print(line, flush=True)


if __name__ == "__main__":
    main()
