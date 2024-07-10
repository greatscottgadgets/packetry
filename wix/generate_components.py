from contextlib import redirect_stdout
import os

dll_components = open('wix/dll-components.wxi', 'w')
dll_references = open('wix/dll-references.wxi', 'w')
license_components = open('wix/license-components.wxi', 'w')
license_references = open('wix/license-references.wxi', 'w')

output_files = [
    dll_components,
    dll_references,
    license_components,
    license_references
]

def component_name(filename):
    return filename.replace('-', '_').replace('+', '_')

for file in output_files:
    print("<Include>", file=file)

bin_dir = '$(env.VCPKG_INSTALLED_DIR)/x64-windows/bin'

for line in open('wix/required-dlls.txt', 'r'):
    filename, guid = line.rstrip().split(' ')
    component = component_name(filename)
    with redirect_stdout(dll_components):
        print(f"    <Component Id='{component}' Guid='{guid}'>")
        print(f"        <File Id='{component}'")
        print(f"              Name='{filename}'")
        print(f"              DiskId='1'")
        print(f"              Source='{bin_dir}/{filename}'/>")
        print(f"    </Component>")
    with redirect_stdout(dll_references):
        print(f"    <ComponentRef Id='{component}'/>")

for filename in os.listdir('wix/full-licenses'):
    component = component_name(filename)
    with redirect_stdout(license_components):
        print(f"    <Component Id='{component}' Guid='*'>")
        print(f"        <File Id='{component}'")
        print(f"              Name='{filename}'")
        print(f"              DiskId='1'")
        print(f"              Source='wix/full-licenses/{filename}'/>")
        print(f"    </Component>")
    with redirect_stdout(license_references):
        print(f"    <ComponentRef Id='{component}'/>")

for file in output_files:
    print("</Include>", file=file)
