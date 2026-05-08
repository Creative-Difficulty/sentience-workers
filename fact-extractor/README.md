# fact-extractor

This microservice pulls messages from the postgres (unidb) database to infer facts, activities and emotions from users' messages.

I have tried to keep it short but the extractor.rs is quite messy due to me trying to provide all the context the LLM could need (previous and later messages as context, channel name, discord server name).
