-- What the assistant **did** that turn, not just what it said at the end.
--
-- It started as a bug that looked like a prompting problem: following up on the previous turn
-- with "translate", the assistant re-ran the whole round of tools, and because there were eight
-- entities with the same name the second pass landed on a different set of them -- what was
-- asked for was the same passage in another language, what came back was different content.
--
-- The real cause was not the prompt. `conversations::recent_context` returns `(role, content)`:
-- within one turn the model can see what it called (those messages are still in `msgs`), but
-- **across turns all of that is gone**, leaving only the prose it wrote itself. So on the next
-- turn it does not know that it already looked things up -- looking them up again is the most
-- reasonable judgement available to it. Using the prompt to push down a correct inference wins
-- some of the time: three out of four in practice.
--
-- So do not throw it away. This column stores that turn's complete message tail -- the
-- assistant message carrying `tool_calls`, plus the matching tool result messages -- and sends
-- it back verbatim on replay.
ALTER TABLE conversation_messages
    -- Shaped like `[{"role":"assistant","tool_calls":[...]}, {"role":"tool","tool_call_id":...}, ...]`.
    --
    -- **Store the copy that was already truncated**: tool results are cut down to
    -- `TOOL_CHUNK_CHARS` before they go to the model, and what is stored here is that same
    -- copy, not the raw return value. Storing the raw one would make a replay take up more
    -- room than the original turn did.
    --
    -- Only meaningful on assistant rows; on user rows it is always an empty array
    ADD COLUMN tool_exchange JSONB NOT NULL DEFAULT '[]'::jsonb;

-- **Only the most recent turn is replayed.** This column exists so the model knows "what I
-- just did", not to haul twenty turns of tool output back into the context -- which is exactly
-- why only the prose was stored in the first place.
-- So there is no index: a query already carries conversation_id and the time ordering.
COMMENT ON COLUMN conversation_messages.tool_exchange IS
    'The assistant turn''s tool calls and their results, replayed for the most recent turn so the model knows what it already did.';
