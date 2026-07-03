name := 'cosmic-nas-indicator'
appid := 'io.github.sbj_ee.CosmicNasIndicator'

build:
    cargo build --release

install: build
    install -Dm0755 target/release/{{name}} ~/.local/bin/{{name}}
    install -Dm0644 data/{{appid}}.desktop ~/.local/share/applications/{{appid}}.desktop

uninstall:
    rm -f ~/.local/bin/{{name}} ~/.local/share/applications/{{appid}}.desktop
