"""uncompyle6 entry point with a fix for Python 2.x constant collections.

uncompyle6 collapses runs of >=5 LOAD_CONSTs feeding a BUILD_LIST/SET/MAP into
ADD_VALUE tokens, copying the LOAD_CONST token's `attr`.  semantics.n_const_list
then prints `attr` for non-string constants.  scanner3 stores the constant
value in `attr`, but scanner2 stores the co_consts *index* there (the value is
in `pattr`), so on 2.x bytecode every such literal decompiles as [1, 2, 3, ...]:
`__notifyevents__ = [1, 2, 3, 4, 5]` instead of the event names.  Rewriting
attr to the constant value restores the real literal.

Second fix: Python 2.2 list comprehensions. 2.2 stores the list's bound
`append` in `_[1]` and adds each element with `_[1](expr)`, parsed as
`lc_body ::= LOAD_FAST expr CALL_FUNCTION_1 POP_TOP`. The printer
(gencomp.comprehension_walk_newer) emits child 0 of lc_body for every 2.x
version, which here is the `_[1]` load, so `[each[0] for each in x]` came out
as `[_[1] for each in x]`. Dropping the leading load makes child 0 the element.
"""
import sys

from uncompyle6 import scanner as _scanner
from uncompyle6.semantics import n_actions as _n_actions

_orig = _scanner.Scanner.bound_collection_from_tokens


def _bound_collection_from_tokens(self, tokens, t, i, collection_type):
    new_tokens = _orig(self, tokens, t, i, collection_type)
    if new_tokens is not None and self.version[:1] == (2,):
        for tok in new_tokens:
            if tok.kind == 'ADD_VALUE':
                tok.attr = tok.pattr
    return new_tokens


_scanner.Scanner.bound_collection_from_tokens = _bound_collection_from_tokens


def _strip_append_loads(node):
    if getattr(node, 'kind', None) == 'lc_body' and len(node) >= 3 \
            and getattr(node[0], 'kind', None) in ('LOAD_FAST', 'LOAD_NAME') \
            and str(getattr(node[0], 'pattr', '')).startswith('_['):
        del node[0]
    # spark AST nodes are UserLists, not lists; tokens are not iterable.
    if hasattr(node, 'data') and isinstance(node.data, list):
        for child in node:
            _strip_append_loads(child)


_orig_list_comp = _n_actions.NonterminalActions.n_list_comp


def _n_list_comp(self, node):
    if self.version[:1] == (2,):
        _strip_append_loads(node)
    return _orig_list_comp(self, node)


_n_actions.NonterminalActions.n_list_comp = _n_list_comp

if __name__ == '__main__':
    from uncompyle6.bin.uncompile import main_bin
    sys.exit(main_bin())
