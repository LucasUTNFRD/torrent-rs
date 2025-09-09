#!/bin/bash
set -e

echo "Starting Transmission seeder..."

# Install transmission-cli if not present
if ! command -v transmission-cli &>/dev/null; then
	echo " Installing transmission-cli..."
	apk add --no-cache transmission-cli
fi

echo "  Waiting for network and tracker..."
sleep 5

# Check if torrent file exists
if [ ! -f /data/test.torrent ]; then
	echo "Torrent file not found at /data/test.torrent"
	exit 1
fi

# Check if data file exists
if [ ! -f /data/README.md ]; then
	echo "Data file not found at /data/README.md"
	exit 1
fi

echo "Found torrent and data files"
echo "Starting to seed test.torrent..."
echo "Seeding on port 51413"

cd /data
transmission-cli \
	--port 51413 \
	--verify \
	test.torrent
