import os
import sys
import types

# speech runs as a flat module from its own directory; mirror that for the tests.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# Only the transcriber thread touches Vosk; a stub lets the module import
# without the native library or a model on disk.
sys.modules.setdefault("vosk", types.ModuleType("vosk"))
