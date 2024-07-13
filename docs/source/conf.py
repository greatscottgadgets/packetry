import sphinx_rtd_theme

extensions = [
    'sphinx_rtd_theme'
]

# -- Project information -----------------------------------------------------

project = 'Packetry'
copyright = '2023-2024, Great Scott Gadgets'
author = 'Great Scott Gadgets'

version = ''
release = ''


# -- General configuration ---------------------------------------------------

extensions = [
    'sphinx.ext.autodoc',
    'sphinx_inline_tabs',
]

templates_path = ['_templates']
exclude_patterns = ['_build']
source_suffix = '.rst'
master_doc = 'index'
language = 'en'
exclude_patterns = []
pygments_style = None


# -- Options for HTML output -------------------------------------------------
# run pip install sphinx_rtd_theme if you get sphinx_rtd_theme errors
html_theme = "sphinx_rtd_theme"
html_css_files = ['status.css']
