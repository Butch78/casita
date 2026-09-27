import runpy
import unittest
from pathlib import Path
from unittest.mock import patch

from benchmarks import cli


class NativePackageTests(unittest.TestCase):
    def test_only_nix_apple_iconv_is_replaced(self):
        package = runpy.run_path(str(cli.ROOT / 'crates/casita-fskit/package.py'))
        binary = Path('/tmp/extension')
        apple = '/nix/store/hash-libiconv-113/lib/libiconv.2.dylib'
        for dependency, replace in [
            (apple, True),
            ('/usr/lib/libiconv.2.dylib', False),
            ('/nix/store/hash-libiconv-1.18/lib/libiconv.2.dylib', False),
        ]:
            with self.subTest(dependency=dependency), patch('subprocess.check_output') as listing, patch('subprocess.run') as change:
                listing.return_value = f'{binary}:\n\t{dependency} (compatibility version 7.0.0)\n'
                package['use_system_iconv'](binary)
                if replace:
                    change.assert_called_once_with(
                        ['install_name_tool', '-change', apple, '/usr/lib/libiconv.2.dylib', str(binary)],
                        check=True,
                    )
                else:
                    change.assert_not_called()
