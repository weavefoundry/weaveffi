#!/usr/bin/env bash
# Conformance lanes for the dotnet target. Run through conformance/run.sh,
# which builds the sample producers, generates bindings, and exports the
# shared environment (see conformance/lib.sh); each lane compiles and runs one
# consumer against one sample and must exit 0.
set -uo pipefail
. "$(dirname "$0")/../lib.sh"
require_tools dotnet dotnet

# .NET: a console app with a ProjectReference to the generated project, used
# as generated. The generated loader finds the producer through
# {PREFIX}_LIBRARY. The app targets the installed SDK's framework so it runs
# on the installed runtime.
dotnet_consumer() {
    local sample="$1" src="$2"
    local proj="$OUT/dotnet-$sample"
    local tfm csproj
    tfm="net$(dotnet --version | cut -d. -f1).0"
    csproj=$(ls "$GENROOT/$sample/dotnet"/*.csproj | head -1)
    rm -rf "$proj"
    mkdir -p "$proj"
    cp "$ROOT/conformance/dotnet/$src" "$proj/Program.cs"
    cp "$ROOT/conformance/dotnet/LeakCheck.cs" "$proj/LeakCheck.cs"
    cat > "$proj/conformance.csproj" <<EOF
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>$tfm</TargetFramework>
    <Nullable>disable</Nullable>
    <AllowUnsafeBlocks>true</AllowUnsafeBlocks>
    <ImplicitUsings>disable</ImplicitUsings>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="$csproj" />
  </ItemGroup>
</Project>
EOF
    ( cd "$proj" \
        && env "$(library_env "$sample")=$(sample_lib "$sample")" \
           dotnet run -c Release --nologo -v quiet 2>&1 )
}

dotnet_calculator() { dotnet_consumer calculator Calculator.cs; }
dotnet_codec() { dotnet_consumer codec Codec.cs; }
dotnet_kvstore() { dotnet_consumer kvstore Kvstore.cs; }

lane dotnet-calculator dotnet_calculator
lane dotnet-codec dotnet_codec
lane dotnet-kvstore dotnet_kvstore

finish_lanes
