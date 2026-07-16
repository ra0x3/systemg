FROM sysg-usecase-base

COPY tests/docker/usecase/inspect-not-found/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/inspect-not-found/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
