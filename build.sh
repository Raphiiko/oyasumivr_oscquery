rm lib/mdns-sidecar.exe
cd src-mdns-sidecar
dotnet publish -r win-x64 -c Release
cd ..
mv src-mdns-sidecar/bin/Release/net8.0/win-x64/publish/mdns-sidecar.exe lib/mdns-sidecar.exe
