FROM sysg-usecase-base

COPY tests/docker/usecase/purge-no-supervisor-clean/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/purge-no-supervisor-clean/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
