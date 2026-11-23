name := 'cosmic-nas-indicator'
appid := 'io.github.sbj_ee.CosmicNasIndicator'

build:
    cargo build --release

test:
    cargo test

# Installs to ~/.local. The desktop entry's Exec= is rewritten to the absolute
# binary path, because the panel's PATH may not include ~/.local/bin.
install: build
    install -Dm0755 target/release/{{name}} ~/.local/bin/{{name}}
    install -d ~/.local/share/applications
    sed "s|^Exec=.*|Exec=$HOME/.local/bin/{{name}}|" data/{{appid}}.desktop > ~/.local/share/applications/{{appid}}.desktop

uninstall:
    rm -f ~/.local/bin/{{name}} ~/.local/share/applications/{{appid}}.desktop
