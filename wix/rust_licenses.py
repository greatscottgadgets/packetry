from license_expression import get_spdx_licensing, LicenseSymbol, OR, AND
from tempfile import TemporaryDirectory
import subprocess
import shutil
import os

licensing = get_spdx_licensing()

# We accept these licenses unconditionally.
accepted_license_strings = (
    'MIT',
    'BSD-2-Clause',
    'BSD-3-Clause',
    'BSL-1.0',
    'Unicode-DFS-2016',
)

# We accept these licenses with manual validation.
conditional_license_strings = (
    # For Apache-2.0, we need to check for NOTICE requirements.
    'Apache-2.0',
    'Apache-2.0 WITH LLVM-exception',
)

# These packages have been validated for conditionally accepted licenses.
validated = (
    ('target-lexicon', '0.12.14'),
    ('unicode-ident', '1.0.12'),
)

# We don't believe these packages are actually linked into Packetry.
excluded = (
    'fuchsia-zircon',
    'fuchsia-zircon-sys',
)

accepted_licenses = [licensing.parse(s) for s in accepted_license_strings]
conditional_licenses = [licensing.parse(s) for s in conditional_license_strings]

# Validate a license expression is acceptable to us.
def validate_license(package, version, expr):

    # If there is a single license, and it's acceptable, accept it.
    if expr in accepted_licenses:
        return True

    # If there is a single conditionally accepted license, and we've validated
    # it for this specific package version, accept it.
    elif expr in conditional_licenses and (package, version) in validated:
        return True

    # If the expression is an OR, and some option is acceptable, accept it.
    elif isinstance(expr, OR):
        for option in expr.get_symbols():
            if validate_license(package, version, option):
                return True
        else:
            return False

    # If the expression is an AND, and all elements are acceptable, accept it.
    elif isinstance(expr, AND):
        for element in expr.get_symbols():
            if not validate_license(package, version, element):
                return False
        else:
            return True
    else:
        # Otherwise, return false.
        return False

# Get license information with 'cargo license'.
cargo_result = subprocess.run(
    ('cargo', 'license', '--authors', '--do-not-bundle', '--color=never'),
    capture_output=True)

cargo_result.check_returncode()

# Unpack sources into temporary directory with 'cargo vendor'.
deps = TemporaryDirectory()
subprocess.run(
    ('cargo', 'vendor', '--versioned-dirs', deps.name)).check_returncode()

# Manually collected licenses.
manual_dir = 'wix/manual-licenses'
manual_licenses = os.listdir(manual_dir)

# Output path for license files.
dest_dir = 'wix/full-licenses'

try:
    os.mkdir(dest_dir)
except FileExistsError:
    pass

print("The following Rust packages are statically linked into Packetry:")

for line in cargo_result.stdout.decode().rstrip().split("\n"):
    package, remainder = line.split(": ")
    if package == 'packetry' or package in excluded:
        continue
    version, license_quoted, by, authors_quoted = remainder.split(", ")
    authors = authors_quoted[1:-1].split("|")
    license_str = license_quoted[1:-1]

    # Check the license is acceptable for us.
    if not validate_license(package, version, licensing.parse(license_str)):
        raise ValueError(
            f"License '{license_str}' not accepted for {package} {version}.")

    # Where we will write the full license file to.
    dest_filename = f'LICENSE-{package}-{version}.txt'
    dest_path = os.path.join(dest_dir, dest_filename)

    # Look for a manually collected license.
    if dest_filename in manual_licenses:
        src_path = os.path.join(manual_dir, dest_filename)
    else:
        # Look for a license file.
        file_paths = (
            ['LICENSE-MIT'],
            ['LICENSE-MIT.md'],
            ['license-mit'],
            ['LICENSES', 'MIT.txt'],
            ['LICENSE'],
            ['LICENSE.txt'],
            ['LICENSE.md'],
            ['COPYING'],
        )
        src_dir = os.path.join(deps.name, f'{package}-{version}')
        for file_path in file_paths:
            src_path = os.path.join(src_dir, *file_path)
            if os.path.isfile(src_path):
                break
        else:
            raise ValueError(
                f"No license file found for {package} {version}.")

    # Copy license file to output.
    shutil.copyfile(src_path, dest_path)

    print()
    print(f"{package} version {version}")
    if len(authors) == 1:
        print(f"Author: {str.join(', ', authors)}")
    else:
        print("Authors:")
        for author in authors:
            print(f"    {author}")
    print(f"License type: {license_str}")
    print(f"License file: full-licenses/{dest_filename}")
    print(f"Link: https://crates.io/crates/{package}")
