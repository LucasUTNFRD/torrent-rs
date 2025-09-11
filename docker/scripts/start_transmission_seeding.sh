#!/bin/bash
set -e

echo "=== Starting Transmission Seeder ==="
echo "Container: $(hostname)"
echo "Time: $(date)"

# Install transmission-cli (Alpine)
echo "Installing transmission-cli..."
apk add --no-cache transmission-cli netcat-openbsd

echo "Waiting for tracker to be ready..."
sleep 10

# Test tracker connectivity
echo "Testing tracker connectivity..."
if nc -z tracker 8000; then
	echo "✅ Tracker is reachable at tracker:8000"
else
	echo "❌ Cannot reach tracker at tracker:8000"
fi

# List available files
echo "Available files in /data:"
ls -la /data/

# Check if required files exist
if [ ! -f /data/test.torrent ]; then
	echo "❌ Torrent file not found at /data/test.torrent"
	exit 1
fi

if [ ! -f /data/README.md ]; then
	echo "❌ Data file not found at /data/README.md"
	exit 1
fi

echo "✅ Found torrent and data files"
echo "🌱 Starting transmission-cli exactly like your local setup..."
echo "Command: transmission-cli -w /data test.torrent"

cd /data

# test.torrent is the torrent file (also in /data)
exec transmission-cli -w /data test.torrent
