Remove items when the work is complete.

Design notes live in the documentation.
`poke_rust/src/solver/README.md` explains the solver targets, the algorithms,
and the analysis jobs.

# New features

- I think we want to cap at depth 1 and just improve the root node evaluator lowk, train that to be as good as possible and solve based on that. Remove the sizing amounts and just make the root node evaluators as good as possible, even if they are slower. (What info can we give them, can we make the architecture better?)
