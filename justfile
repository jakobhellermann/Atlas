set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

[private]
default:
    @just --list --unsorted

build:
    cargo build --release

clean:
    rm Atlas.msi Atlas.wixpdb

[unix]
package: build
    cargo bundle --release

[windows]
package: build
    wix build ./packaging/msi/Package.wxs -out Atlas

[windows]
install: package
    ./Atlas.msi