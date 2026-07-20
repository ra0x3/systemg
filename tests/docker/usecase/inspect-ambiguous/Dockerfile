FROM sysg-usecase-base

COPY tests/docker/usecase/inspect-ambiguous/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/inspect-ambiguous/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
