//! Answer shape and harvesting logic was removed in the LLM-driven loop
//! refactor. The LLM is now responsible for producing the correct JSON shape
//! and extracting values from sandbox output — the bot only parses ```code
//! blocks```, executes them, looks for `ANSWER:`, and submits via
//! `answer_wire_payload` (ensure valid JSON).
