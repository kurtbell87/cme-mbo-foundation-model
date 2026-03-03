# Domain Priors

Knowledge injected by the research lead. The SURVEY and FRAME agents
MUST read this file and incorporate these priors into experiment design.

## Problem Structure
- _What structural properties does the problem have?_
- _e.g., "Board games have 2D spatial structure. CNN or attention over board positions is strongly preferred over flat MLP for any opponent beyond trivial difficulty."_

## Known Architecture-Problem Mappings
- _What architectures have practitioners found effective for this problem class?_
- _e.g., "AlphaZero used residual CNN + MCTS. For our PPO-only approach, at minimum test CNN encoding of the board."_

## Anti-Patterns to Avoid
- _What should the research pipeline NOT waste time on?_
- _e.g., "Don't tune MLP hyperparameters hoping to crack depth-4. If MLP fails at depth-4, the answer is architecture, not learning rate."_

## Domain-Specific Guidance
- _Any other domain knowledge that should guide experiment design._
- _e.g., framework conventions, evaluation protocols, known baselines._
