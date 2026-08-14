# The README demo, recorded with https://github.com/charmbracelet/vhs.
#
#   cargo build --release
#   docker compose -p storefront -f docs/demo/storefront.yml up -d
#   docker compose -p analytics  -f docs/demo/analytics.yml  up -d
#   vhs docs/demo.tape
#   docker compose -p storefront -f docs/demo/storefront.yml down -v
#   docker compose -p analytics  -f docs/demo/analytics.yml  down -v
#
# It deliberately stays on the containers tab. The images and volumes tabs list
# whatever happens to be on the recording machine, which is nobody's business
# but its own.
Output "docs/demo.gif"

Set Shell "bash"
Set FontSize 15
Set Width 1280
Set Height 680
Set Padding 18
Set Framerate 24
Set TypingSpeed 55ms

Hide
Type "export PS1='$ ' PATH=$PWD/target/release:$PATH && clear"
Enter
Sleep 1s
Show

Type "dockmate"
Enter

# Let the first poll land and the stats streams produce their second frame,
# which is where the cpu and memory numbers appear.
Sleep 4s

# Move down one — the log pane follows the selection.
Down
Sleep 3s

# The detail pane.
Enter
Sleep 3500ms
Enter
Sleep 1s

# Group by compose project. Stacked mode opens with every stack folded.
Type "z"
Sleep 3s

# Unfold them all.
Type "Z"
Sleep 3s

# Filter for a service two different stacks both run — the payoff of grouping
# on the label rather than guessing from the container name.
Type "/"
Sleep 400ms
Type "postgres"
Sleep 4s
Escape
Sleep 1500ms

Type "q"
Sleep 800ms
