#!/bin/bash

cd "$(dirname "$0")/.."


# Check if only one argument is provided
if [ "$#" -ne 1 ]; then
    echo "Usage: $0 path_to_notice_file"
    exit 1
fi

# Assign notice_file_path to argument
notice_file_path="$1"

# Check if the notice file exists
if [ ! -f "$notice_file_path" ]; then
    echo "Error: Notice file '$notice_file_path' not found"
    exit 1
fi

DOTNET_DIRECTORY="cloud_connectors/azure/digital_twins_connector"

dotnet tool install --global dotnet-project-licenses
mkdir -p "$DOTNET_DIRECTORY/dotnet_licenses_output"
dotnet-project-licenses -i $DOTNET_DIRECTORY -o -f "$DOTNET_DIRECTORY/dotnet_licenses_output" -u --json -e -c \
--licenseurl-to-license-mappings "$DOTNET_DIRECTORY/license_url_to_type.json"
./tools/dotnet_get_licenses.sh "$DOTNET_DIRECTORY/dotnet_licenses_output/licenses.json" "$DOTNET_DIRECTORY/dotnet_licenses_output"
./tools/dotnet_append_to_notice.sh "$notice_file_path" "$DOTNET_DIRECTORY/dotnet_licenses_output/licenses.json"
rm -rf "$DOTNET_DIRECTORY/dotnet_licenses_output"