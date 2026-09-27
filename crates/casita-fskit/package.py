"""Build and package the native repository extension. Registration is done by Rust setup."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess

def use_system_iconv(binary):
    # Nix's Darwin compiler wrapper can select its copy of Apple's libiconv.
    # Hardened extensions cannot load that ad-hoc signed store library. Use
    # the system ABI instead, before signing; leave library validation enabled.
    linked = subprocess.check_output(['otool', '-L', str(binary)], text=True)
    for line in linked.splitlines()[1:]:
        dependency = line.strip().split(' (', 1)[0]
        if re.fullmatch(r'/nix/store/[^/]+-libiconv-[0-9]+/lib/libiconv\.2\.dylib', dependency):
            subprocess.run(['install_name_tool', '-change', dependency,
                            '/usr/lib/libiconv.2.dylib', str(binary)], check=True)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--identity', default='-', help='Signing identity; - is development ad-hoc signing')
    args = parser.parse_args()
    source = Path(__file__).resolve().parent
    target = Path(os.environ.get('CASITA_NATIVE_TARGET_DIR', source / 'target')).resolve()
    app = args.output.resolve()
    if app.exists():
        parser.error('output must not exist; installed bundles must not be overwritten while mounted')
    subprocess.run(['cargo', 'build', '--locked', '--release', '--manifest-path', str(source/'Cargo.toml'),
                    '--features', 'production', '--bin', 'casita-native-fskit', '--bin', 'casita-native-fskit-extension',
                    '--target-dir', str(target)], check=True)
    extension = app/'Contents/Extensions/casita-native-fskit-extension.appex'
    hashes = {}
    for kind, bundle, binary in [('extension', extension, 'casita-native-fskit-extension'), ('host', app, 'casita-native-fskit')]:
        (bundle/'Contents/MacOS').mkdir(parents=True, exist_ok=True)
        shutil.copy2(target/'release'/binary, bundle/'Contents/MacOS'/binary)
        use_system_iconv(bundle/'Contents/MacOS'/binary)
        info = plistlib.loads((source/kind/'Info.plist').read_bytes())
        info.update(CFBundleIdentifier='org.casita.fskit.extension' if kind=='extension' else 'org.casita.fskit',
                    CFBundleDisplayName='Casita FSKit', LSMinimumSystemVersion='26.0')
        if kind=='extension':
            info['EXAppExtensionAttributes'].update(FSShortName='casita', FSSupportsBlockResources=False,
                                                    FSSupportsPathURLs=True, FSRequiresSecurityScopedPathURLResources=True)
        (bundle/'Contents/Info.plist').write_bytes(plistlib.dumps(info))
        entitlements = 'adhoc.entitlements' if kind=='extension' and args.identity=='-' else 'main.entitlements'
        subprocess.run(['codesign', '--force', '--sign', args.identity, '--options', 'runtime',
                        *(['--timestamp=none'] if args.identity=='-' else ['--timestamp']),
                        '--entitlements', str(source/kind/entitlements), '--generate-entitlement-der', str(bundle)], check=True)
        subprocess.run(['codesign', '--verify', '--strict', str(bundle)], check=True)
        hashes[binary] = hashlib.sha256((bundle/'Contents/MacOS'/binary).read_bytes()).hexdigest()
    print(json.dumps({'bundle':str(app), 'signing':args.identity, 'binaries':hashes, 'notarized':False}, indent=2))

if __name__ == '__main__':
    main()
