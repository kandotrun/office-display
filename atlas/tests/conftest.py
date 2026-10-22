import os
import sys

# atlas runs as flat modules from its own directory; mirror that for the tests.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
