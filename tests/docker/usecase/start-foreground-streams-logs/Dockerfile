FROM sysg-usecase-base

COPY tests/docker/usecase/start-foreground-streams-logs/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/start-foreground-streams-logs/fg.py /usecase/fg.py
COPY tests/docker/usecase/start-foreground-streams-logs/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
