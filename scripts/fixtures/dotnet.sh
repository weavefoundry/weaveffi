#!/usr/bin/env bash
# Build the generated .NET project with warnings as errors. The project
# targets net8.0; restore needs that reference pack (cached by any earlier
# net8.0 restore, or downloaded from NuGet).
set -euo pipefail
dir=$1
csproj=$(ls "$dir"/dotnet/*.csproj | head -1)
dotnet build "$csproj" -warnaserror -nologo -v quiet \
    -p:BaseOutputPath="$dir/dotnet-bin/" -p:BaseIntermediateOutputPath="$dir/dotnet-obj/"
