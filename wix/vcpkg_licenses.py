import subprocess
import shutil
import json
import re
import os

dest_dir = 'wix/full-licenses'

vcpkg_result = subprocess.run(
    ['./vcpkg/vcpkg', 'depend-info', 'gtk', '--triplet=x64-windows'],
    capture_output=True)

vcpkg_result.check_returncode()

packages = set()

# Find all packages we depend on.
for line in vcpkg_result.stderr.decode().rstrip().split('\n'):
    header, remainder = line.split(': ')
    package = header.split('[')[0]
    packages.add(package)
    dependencies = remainder.split(', ')
    packages |= set(dependencies)

# Discard empty package names.
packages.discard('')

# Discard vcpkg build tools.
packages = set(p for p in packages if not p.startswith('vcpkg-'))

# gperf is needed to build fontconfig, but is not linked to.
packages.discard('gperf')

# gettext is needed to build many dependencies, but is not linked to.
packages.discard('gettext')

# getopt and pthread are virtual packages.
packages.discard('getopt')
packages.discard('pthread')

# sassc is used to build GTK, but is not linked to.
packages.discard('sassc')

versions = {}

# These packages are missing license information in vcpkg.
licenses = {
    'libiconv': 'LGPL-2.1-or-later',
    'egl-registry': 'Apache-2.0 OR MIT',
    'liblzma': 'BSD-0-Clause',
    'libsass': 'MIT',
}

# These packages are missing homepage information in vcpkg.
homepages = {
    'fribidi': 'https://github.com/fribidi/fribidi',
}

for package in packages:
    metadata = json.load(open(f'vcpkg/ports/{package}/vcpkg.json'))
    version = metadata.get('version-semver',
        metadata.get('version',
            metadata.get('version-date')))
    if version is None:
        raise ValueError(f"Couldn't find a version for package {package}")
    versions[package] = version
    if metadata.get('homepage') is not None:
        homepages[package] = metadata['homepage']
    if metadata.get('license') is not None:
        licenses[package] = metadata['license']

try:
    os.mkdir(dest_dir)
except FileExistsError:
    pass

print("The following libraries are dynamically linked into Packetry:")

for package in sorted(packages):
    license_src = f'vcpkg/installed/x64-windows/share/{package}/copyright'
    license_dest = os.path.join(dest_dir, f'LICENSE-{package}.txt')
    shutil.copyfile(license_src, license_dest)
    print()
    print(f"{package} version {versions[package]}")
    print(f"Homepage: {homepages[package]}")
    print(f"License type: {licenses[package]}")
    print(f"License text: {license_dest}")
