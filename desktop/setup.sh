#!/bin/bash
#
# Homeboy - Python Environment Setup
#
# This script sets up the Python virtual environment required
# for the Bandcamp scraper module.
#

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VENV_DIR="$SCRIPT_DIR/venv"

echo "========================================"
echo "Homeboy - Python Setup"
echo "========================================"

# Check for Python
if ! command -v python3 &> /dev/null; then
    echo "Error: Python 3 is not installed."
    echo "Install it with: brew install python"
    exit 1
fi

PYTHON_VERSION=$(python3 --version)
echo "Found: $PYTHON_VERSION"

# Create virtual environment
if [ -d "$VENV_DIR" ]; then
    echo "Virtual environment already exists at $VENV_DIR"
    read -p "Recreate it? (y/n) " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        rm -rf "$VENV_DIR"
    else
        echo "Skipping venv creation."
    fi
fi

if [ ! -d "$VENV_DIR" ]; then
    echo "Creating virtual environment..."
    python3 -m venv "$VENV_DIR"
    echo "Virtual environment created."
fi

# Activate and install dependencies
echo "Installing dependencies..."
source "$VENV_DIR/bin/activate"

pip install --upgrade pip
pip install playwright beautifulsoup4 lxml requests tldextract

echo "Installing Playwright browsers..."
python -m playwright install chromium

echo ""
echo "========================================"
echo "Setup complete!"
echo "========================================"
echo ""
echo "To use manually:"
echo "  source $VENV_DIR/bin/activate"
echo "  python Homeboy/Resources/Scripts/bandcamp_scraper.py --help"
echo ""
