# A fake OpenAI-compatible endpoint for tests

Card: headway:notedeck/display-shine-carry
Native-loop and sub-agent tests each hand-roll a one-shot TcpListener,
~30 lines apiece, none of them streaming-aware.

- headway:dave/deputy-trust-license — ~5 min: wrote a third one-shot server (after first_request and one_completion) to stream a tool call and a usage chunk as SSE; a `FakeOpenAi::streaming(chunks)` / `::completion(body)` in test_support handing back the request it got would have left only the chunks and the asserts
