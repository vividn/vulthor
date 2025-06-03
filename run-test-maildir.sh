#!/bin/bash

# Create a temporary directory
TEMP_DIR=$(mktemp -d)

# Function to cleanup on exit
cleanup() {
    echo "Cleaning up temporary MailDir at $TEMP_DIR..."
    rm -rf "$TEMP_DIR"
}

# Set up trap to cleanup on exit
trap cleanup EXIT

# Copy the fixture maildir to temp directory
echo "Copying fixture MailDir to temporary location: $TEMP_DIR"
cp -r fixture/maildir/* "$TEMP_DIR/"

# Run vulthor with the temporary maildir
echo "Starting Vulthor with test MailDir..."
cargo run -- -m "$TEMP_DIR" "$@"

# The cleanup function will run automatically on exit