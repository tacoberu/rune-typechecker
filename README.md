Rune TypeChecker
================

I have Rune in my system.
Users write scripts that I then process.
The problem is that a user can completely ignore the contract: the function they wrote returns values it should not,
values we never agreed on, and I have no way to check that what they wrote is correct.
Naturally, I want to check it before I allow the script to be saved.


I'm considering writing a checker that verifies a user-written function satisfies its contract.
I would use typed doc-comments for it —
the way it is done in PHP with phpstan, for example.
